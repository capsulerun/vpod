"""uv vs warm-pip install benchmark.

Each measurement runs in a FRESH sandbox so neither tool sees the other's
already-installed packages or download cache. The daemon is warmed once per
sandbox before timing so we measure warm-pip (not cold-pip) against uv. Both
tools run with their caches disabled and against the same index, so the only
variable is the installer itself.
"""

import statistics
import time

from vpod import Sandbox

# Pure-Python wheels, increasing dependency counts. No compiler, no big wheels
# (those are the OOM story, not the install-machinery story we're measuring).
PACKAGES = ["six", "requests", "flask"]
REPEATS = 1

# --no-cache on both so the second tool can't cheat off the first's downloads;
# --system so uv installs into the guest's system site-packages (no venv here).
# uv must be pointed at the real (dynamically linked) interpreter: the default
# `python` on PATH is our static musl shim, whose ELF has no PT_INTERP, so uv
# can't detect the libc and refuses. Same reason pip's children use python3.real.
INSTALLERS = {
    # "warm-pip": "pip install --no-cache-dir {pkg}",
    "uv": "uv pip install --no-cache --system {pkg}",
}


def measure_install(installer_cmd, pkg):
    """One pristine run: fresh sandbox, warm the daemon, time the install."""
    with Sandbox.create() as sandbox:
        # Warm the prefork daemon so pip's startup is the warm path, and settle
        # the TLS proxy / DNS before the clock starts.
        sandbox.commands.run("python3 -c pass")
        sandbox.commands.run("echo warmup")

        cmd = installer_cmd.format(pkg=pkg)
        start = time.monotonic()
        result = sandbox.commands.run(cmd, timeout=300)
        elapsed = time.monotonic() - start

        if not result.success:
            print(f"    FAILED ({result.exit_code}): {cmd}")
            tail = (result.stderr or result.stdout or "").strip().splitlines()[-4:]
            for line in tail:
                print(f"      {line}")
            return None
        return elapsed


def main():
    print(f"repeats={REPEATS}  packages={PACKAGES}\n")
    results = {}  # (installer, pkg) -> [times]

    for pkg in PACKAGES:
        for name, cmd in INSTALLERS.items():
            times = []
            for i in range(REPEATS):
                elapsed = measure_install(cmd, pkg)
                if elapsed is not None:
                    times.append(elapsed)
                    print(f"  {name:>8}  {pkg:<9}  run {i + 1}: {elapsed:6.3f}s")
            results[(name, pkg)] = times
        print()

    print("── median (s) ─────────────────────────────")
    print(f"{'package':<10} {'warm-pip':>10} {'uv':>10} {'winner':>10}")
    for pkg in PACKAGES:
        pip_times = results.get(("warm-pip", pkg), [])
        uv_times = results.get(("uv", pkg), [])
        pip_med = statistics.median(pip_times) if pip_times else float("nan")
        uv_med = statistics.median(uv_times) if uv_times else float("nan")
        if pip_times and uv_times:
            winner = "uv" if uv_med < pip_med else "warm-pip"
        else:
            winner = "?"
        print(f"{pkg:<10} {pip_med:>10.3f} {uv_med:>10.3f} {winner:>10}")


if __name__ == "__main__":
    main()
