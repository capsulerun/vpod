"""Probe the desync family: code.run size limit, prompt collision, SIGINT-proof timeout."""
from vpod import Sandbox


def markers(sandbox, n=4):
    vals = [sandbox.commands.run(f"echo m{i}", timeout=60).stdout.strip() for i in range(n)]
    return "CLEAN" if vals == [f"m{i}" for i in range(n)] else f"DESYNC {vals!r}"


with Sandbox.create() as sandbox:
    sandbox.commands.run("python3 -c pass")

    print("── 1. code.run size limit ──")
    for lines in (100, 400, 700, 1200):
        src = "\n".join(f"x{i} = {i}" for i in range(lines)) + "\nprint('ok', x0)"
        r = sandbox.code.run(src, timeout=120)
        follow = sandbox.code.run("print('follow')", timeout=60)
        print(f"  src={len(src):5d}B b64={len(src) * 4 // 3:5d}B "
          f"run={'ok' if 'ok 0' in (r.text or '') else 'FAIL ' + repr((r.text or r.error or '')[:60])} "
          f"follow={'ok' if 'follow' in (follow.text or '') else 'FAIL'}")

    print("── 2. prompt collision in stdout ──")
    r = sandbox.commands.run("printf 'x # '", timeout=60)
    print(f"  printf 'x # ': stdout={r.stdout!r} success={r.success}")
    print(f"  session after: {markers(sandbox)}")

    print("── 3. timeout with SIGINT-ignoring program ──")
    r = sandbox.commands.run(
        "python3 -c 'import signal,time; signal.signal(signal.SIGINT, signal.SIG_IGN); time.sleep(60)'",
        timeout=8,
    )
    print(f"  result: success={r.success} exit={r.exit_code} stdout={r.stdout[:40]!r}")
    print(f"  session after: {markers(sandbox)}")
