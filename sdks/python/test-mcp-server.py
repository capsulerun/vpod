"""Run a real MCP server inside the sandbox and talk to it.

The heaviest end-to-end test we have: `uv pip install mcp` pulls a real
dependency tree over the guest network, a FastMCP server (streamable
HTTP transport) runs backgrounded as a daemon inside the guest, and an
MCP client — also inside the guest — does the full protocol dance
against it: initialize handshake, list_tools, call_tool. A pass proves
package installs, guest networking, background processes, localhost
sockets, and asyncio all work together in one session.
"""

import base64
import time

from vpod import Sandbox

SERVER = '''\
from mcp.server.fastmcp import FastMCP

mcp = FastMCP("sandbox-demo", host="127.0.0.1", port=8000)

@mcp.tool()
def add(a: int, b: int) -> int:
    """Add two numbers."""
    return a + b

mcp.run(transport="streamable-http")
'''

CLIENT = '''\
import asyncio
import time

# time the heavy import separately: on an emulated CPU, pulling in
# pydantic/anyio/httpx/starlette is a large one-time cost that has nothing
# to do with per-call latency.
_t = time.monotonic()
from mcp import ClientSession
from mcp.client.streamable_http import streamablehttp_client
t_import = time.monotonic() - _t

N = 10  # steady-state calls timed after warm-up

async def main():
    async with streamablehttp_client("http://127.0.0.1:8000/mcp") as (r, w, _):
        async with ClientSession(r, w) as session:
            t0 = time.monotonic()
            info = await session.initialize()
            t_handshake = time.monotonic() - t0

            t0 = time.monotonic()
            tools = await session.list_tools()
            t_list = time.monotonic() - t0

            # first call pays any lazy connection warm-up; the rest are
            # steady state, which is what an agent making many tool calls
            # against a persistent session actually experiences.
            calls = []
            result = None
            for _ in range(N + 1):
                c0 = time.monotonic()
                result = await session.call_tool("add", {"a": 40, "b": 2})
                calls.append(time.monotonic() - c0)

            first = calls[0]
            rest = sorted(calls[1:])
            median = rest[len(rest) // 2]
            fastest = rest[0]

            print("server:", info.serverInfo.name)
            print("tools:", [t.name for t in tools.tools])
            print("import (one-time):   %.3fs" % t_import)
            print("handshake (one-time):%.3fs" % t_handshake)
            print("list_tools:          %.3fs" % t_list)
            print("first call_tool:     %.3fs" % first)
            print("steady call_tool:    %.3fs  (median of %d)" % (median, N))
            print("fastest call_tool:   %.3fs" % fastest)
            print("add(40, 2) =", result.content[0].text)

asyncio.run(main())
'''


def write_guest_file(sandbox, path, content):
    # no files API in the SDK yet; base64 through the shell avoids all quoting
    encoded = base64.b64encode(content.encode()).decode()
    return sandbox.commands.run(f"echo {encoded} | base64 -d > {path}")


def step(label, result, expect=None):
    out = (getattr(result, "stdout", None) or getattr(result, "text", "")).strip()
    err = (getattr(result, "stderr", None) or getattr(result, "error", None) or "").strip()
    ok = result.success and (expect is None or expect in out)
    print(f"  [{'ok' if ok else 'FAIL'}] {label}")
    if out:
        print("        " + out.replace("\n", "\n        "))
    if not ok and err:
        print("        stderr: " + err[-500:])
    return ok


def main():
    with Sandbox.create() as sandbox:
        # settle the daemon before anything is timed
        sandbox.commands.run("python3 -c pass")

        print("── install ──")
        # cryptography/pydantic-core/rpds-py/cffi have no riscv64 wheels on
        # PyPI, and source-building them needs a Rust toolchain plus more RAM
        # than the guest has. Alpine ships them prebuilt: apk for the native
        # pieces, uv for the pure-Python rest — the split the README suggests.
        t0 = time.monotonic()
        r = sandbox.commands.run(
            "apk add py3-cryptography py3-pydantic py3-pydantic-core"
            " py3-rpds-py py3-cffi",
            timeout=600,
        )
        print(f"  apk add native deps: {time.monotonic() - t0:.1f}s")
        if not r.success:
            print(f"  [FAIL] {(r.stderr or r.stdout).strip()[-500:]}")
            raise SystemExit(1)

        # Alpine ships cryptography 46.0.7 with cffi 1.17.1 and they work
        # together; only the PyPI metadata insists on cffi>=2. Override it so
        # uv keeps the apk pair instead of source-building cffi 2.x.
        t0 = time.monotonic()
        r = sandbox.commands.run(
            "echo 'cffi<2' > /root/overrides.txt"
            " && uv pip install --system --break-system-packages"
            " --override /root/overrides.txt mcp",
            timeout=600,
        )
        print(f"  uv pip install mcp: {time.monotonic() - t0:.1f}s")
        if not r.success:
            print(f"  [FAIL] {(r.stderr or r.stdout).strip()[-500:]}")
            raise SystemExit(1)

        print("\n── server ──")
        write_guest_file(sandbox, "/root/server.py", SERVER)
        if not step(
            "start server (backgrounded)",
            sandbox.commands.run(
                "nohup python3 /root/server.py > /root/server.log 2>&1 & echo started"
            ),
            expect="started",
        ):
            raise SystemExit(1)

        # poll until the port listens instead of sleeping blind; importing
        # the mcp stack (pydantic/starlette/uvicorn) takes a while emulated
        t0 = time.monotonic()
        up = sandbox.commands.run(
            'up=""; for i in $(seq 1 150); do'
            '  python3 -c "import socket; socket.create_connection((\'127.0.0.1\', 8000), 1)"'
            "  2>/dev/null && up=1 && break; sleep 1;"
            'done; if [ -n "$up" ]; then echo listening;'
            "else echo TIMEOUT; cat /root/server.log; false; fi",
            timeout=240,
        )
        if not step("server listening on :8000", up, expect="listening"):
            raise SystemExit(1)
        print(f"  server startup: {time.monotonic() - t0:.1f}s")

        print("\n── client (per-phase breakdown) ──")
        step(
            "bare python3 sanity after poll loop",
            sandbox.commands.run("python3 -c 'print(\"sane\")'"),
            expect="sane",
        )
        w = write_guest_file(sandbox, "/root/client.py", CLIENT)
        if not step(
            "write client.py",
            sandbox.commands.run("wc -c /root/client.py && md5sum /root/client.py"),
        ):
            print(f"        write result: success={w.success}"
                  f" out={w.stdout!r} err={w.stderr!r}")
            raise SystemExit(1)
        t0 = time.monotonic()
        ok = step(
            "MCP handshake + list_tools + 11x call_tool",
            sandbox.commands.run("python3 /root/client.py", timeout=300),
            expect="add(40, 2) = 42",
        )
        # this outer number bundles python spawn + import + handshake + all
        # calls; the per-phase lines printed by the client are what matter.
        # "steady call_tool" is the metric that decides tool-heavy agent UX.
        print(f"  client process total: {time.monotonic() - t0:.1f}s")

        if not ok:
            diag = sandbox.commands.run(
                "echo '--- state ---';"
                " ps | grep -E 'pydaemon|server.py' | grep -v grep;"
                " dmesg | grep -i -E 'oom|killed process' | tail -3;"
                " echo '--- client rerun, raw ---';"
                " python3 /root/client.py; echo exit=$?;"
                " echo '--- server.log tail ---'; tail -20 /root/server.log",
                timeout=300,
            )
            print("  diagnostics:\n        "
                  + (diag.stdout or diag.stderr or "<empty>").replace("\n", "\n        "))

        if ok:
            sandbox.commands.run("tail -2 /root/server.log")

    print(f"\n{'PASS' if ok else 'FAIL'}")
    raise SystemExit(0 if ok else 1)


if __name__ == "__main__":
    main()
