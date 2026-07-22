"""Probe: does a nohup-backgrounded process survive across commands.run calls?"""
from vpod import Sandbox

with Sandbox.create() as sandbox:
    sandbox.commands.run("python3 -c pass")
    r = sandbox.commands.run(
        "apk add py3-cryptography py3-pydantic py3-pydantic-core py3-rpds-py py3-cffi"
        " && echo 'cffi<2' > /root/overrides.txt"
        " && uv pip install --system --break-system-packages"
        " --override /root/overrides.txt mcp",
        timeout=600,
    )
    print("install:", "ok" if r.success else (r.stderr or r.stdout)[-300:])

    r = sandbox.commands.run(
        "nohup sh -c 'sleep 60' > /root/bg.log 2>&1 & echo started $!"
    )
    print("start:", r.stdout.strip(), "| success:", r.success)

    r = sandbox.commands.run("sleep 1; ps | grep -v grep | grep 'sleep 60' || echo GONE")
    print("after 1s:", r.stdout.strip())

    # also try the python server itself to see its startup error, foreground
    r = sandbox.commands.run(
        "printf 'from mcp.server.fastmcp import FastMCP\\n"
        "mcp = FastMCP(\"d\", host=\"127.0.0.1\", port=8000)\\n"
        "print(\"constructed ok\")\\n' > /root/t.py && timeout 30 python3 /root/t.py 2>&1",
        timeout=60,
    )
    print("fastmcp import/construct:", (r.stdout or r.stderr).strip()[-800:])
