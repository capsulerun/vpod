"""Probe: confirm the daemon child is OOM-killed when the MCP server is resident."""
import base64
from vpod import Sandbox

from importlib.machinery import SourceFileLoader
t = SourceFileLoader("t", "test-mcp-server.py").load_module()

with Sandbox.create() as sandbox:
    sandbox.commands.run("python3 -c pass")
    r = sandbox.commands.run(
        "apk add py3-cryptography py3-pydantic py3-pydantic-core py3-rpds-py py3-cffi"
        " && echo 'cffi<2' > /root/overrides.txt"
        " && uv pip install --system --break-system-packages"
        " --override /root/overrides.txt mcp",
        timeout=600,
    )
    print("install:", "ok" if r.success else "FAIL")

    enc = base64.b64encode(t.SERVER.encode()).decode()
    sandbox.commands.run(f"echo {enc} | base64 -d > /root/server.py")
    enc = base64.b64encode(t.CLIENT.encode()).decode()
    sandbox.commands.run(f"echo {enc} | base64 -d > /root/client.py")

    sandbox.commands.run("nohup python3 /root/server.py > /root/server.log 2>&1 &")
    r = sandbox.commands.run(
        'for i in $(seq 1 150); do'
        ' python3 -c "import socket; socket.create_connection((\'127.0.0.1\', 8000), 1)"'
        " 2>/dev/null && break; sleep 1; done; echo up",
        timeout=240,
    )
    print("server:", r.stdout.strip())

    r = sandbox.commands.run(
        "free -m | head -2; ls -l /run/vpod-pyd.sock 2>&1;"
        " ps | grep -c pydaemon"
    )
    print("before client:\n" + r.stdout)

    r = sandbox.commands.run("python3 /root/client.py; echo exit=$?", timeout=300)
    print("client attempt 1 tail:", (r.stdout or "").strip()[-120:] or "<no stdout>",
          "| stderr:", (r.stderr or "").strip()[-200:] or "<none>")

    r = sandbox.commands.run(
        "dmesg | grep -i -E 'out of memory|oom|killed process' | tail -8;"
        " ls -l /run/vpod-pyd.sock 2>&1; ps | grep pydaemon | grep -v grep || echo daemon-gone"
    )
    print("after client:\n" + r.stdout)
