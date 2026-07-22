"""Verify: long commands go through the staged path without desyncing."""
from vpod import Sandbox

with Sandbox.create() as sandbox:
    sandbox.commands.run("python3 -c pass")

    for size in (2057, 2500, 6000):
        r = sandbox.commands.run(f"echo {'A' * size} | wc -c", timeout=60)
        markers = [
            sandbox.commands.run(f"echo m{i}", timeout=60).stdout.strip()
            for i in range(4)
        ]
        clean = markers == [f"m{i}" for i in range(4)]
        print(f"len={size}: wc={r.stdout.strip()!r} success={r.success} "
              f"{'CLEAN' if clean else 'DESYNC ' + repr(markers)}")
