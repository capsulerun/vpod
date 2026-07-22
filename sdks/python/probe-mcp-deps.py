"""Probe: what does apk provide vs what does uv want to install for mcp."""
from vpod import Sandbox

with Sandbox.create() as sandbox:
    sandbox.commands.run("python3 -c pass")
    r = sandbox.commands.run(
        "apk add py3-cryptography py3-pydantic-core py3-rpds-py py3-cffi py3-pydantic",
        timeout=300,
    )
    print("apk:", "ok" if r.success else (r.stderr or r.stdout)[-300:])

    r = sandbox.commands.run(
        "uv pip list --system 2>/dev/null | grep -i -E 'cffi|pydantic|crypt|rpds'"
    )
    print("installed:\n" + r.stdout)

    r = sandbox.commands.run("python3 --version && uv --version")
    print("versions:", r.stdout.strip())

    r = sandbox.commands.run(
        "uv pip install --system --break-system-packages --dry-run mcp 'cffi<2' 2>&1"
        " | tail -20",
        timeout=300,
    )
    print("with cffi<2 pin:\n" + (r.stdout or r.stderr))
