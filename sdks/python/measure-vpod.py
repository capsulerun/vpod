"""Full-workload benchmark: where vpod actually stands on real agent workloads.

Runs one representative agent session (package install, python tooling, search,
network) inside a single sandbox and times each step against the same work done
natively on the host. The point is the *spread* across workload types, not a
single slowdown multiplier — vpod ranges from ~free to ~100x depending entirely
on what you run, and one number hides that.

Two rules this script exists to enforce, both learned the hard way:

  1. ALWAYS check exit status. A command that fails with 127 returns instantly
     and looks like a spectacular win. Every timing here is discarded unless the
     command actually succeeded.
  2. ALWAYS pin VPOD_SNAPSHOT to dist/. The SDK otherwise defaults to the
     registry-cached vsnap-base, which has no `uv` — so the uv benchmarks would
     silently measure a 127 failure.

Usage:  python3 measure-vpod.py
"""

import os
import shutil
import statistics
import subprocess
import tempfile
import time
from pathlib import Path

# Pin the snapshot before importing the SDK. Absolute, so cwd doesn't matter.
#
# This must be right or the whole run is meaningless: snapshots.pull() silently
# falls back to the registry-cached vsnap-base when VPOD_SNAPSHOT points at a
# path that doesn't exist, and vsnap-base has no `uv` — so every uv benchmark
# would "win" at 0.02s by failing with 127. Fail loudly here instead.
_REPO = Path(__file__).resolve().parents[2]
_SNAP = _REPO / "dist" / "alpine-3.23.0-256mb.snap"

_override = os.environ.get("VPOD_SNAPSHOT")
if _override and Path(_override).resolve() != _SNAP:
    print(f"WARNING: VPOD_SNAPSHOT is set to {_override},")
    print(f"         not the expected {_SNAP}. Using yours.\n")
elif not _SNAP.exists():
    raise SystemExit(
        f"error: snapshot not found: {_SNAP}\n"
        "       Build it first: ./scripts/build-default-snapshot.sh\n"
        "       (without it the SDK falls back to vsnap-base, which has no uv)"
    )
else:
    os.environ["VPOD_SNAPSHOT"] = str(_SNAP)

from vpod import Sandbox  # noqa: E402  (must follow the env pin above)

NET_REPEATS = 3  # network timings are noisy; take a median

# Identical on both sides so ripgrep sees the same bytes in the guest and on the
# host. Generation is never timed — only the search that follows it.
CORPUS = r"""
import os
os.makedirs(root, exist_ok=True)
for f in range(120):
    with open(os.path.join(root, "file%d.txt" % f), "w") as fh:
        for l in range(400):
            fh.write("line %d of file %d lorem ipsum dolor sit amet consectetur\n" % (l, f))
        fh.write("NEEDLE_MARKER_XYZ\n")
print("corpus ready")
"""

results = []  # (group, label, guest_s, native_s | None, note)


def record(group, label, guest_s, native_s=None, note=""):
    results.append((group, label, guest_s, native_s, note))
    ratio = f"{guest_s / native_s:6.1f}x" if native_s else "     —"
    nat = f"{native_s:7.3f}s" if native_s else "      —"
    print(f"    {label:<28} guest {guest_s:7.3f}s   native {nat}   {ratio}  {note}")


def guest_run(sandbox, cmd, timeout=300):
    """Time a guest command. Returns None (and shouts) if it did not succeed."""
    start = time.monotonic()
    result = sandbox.commands.run(cmd, timeout=timeout)
    elapsed = time.monotonic() - start
    if not result.success:
        print(f"    !! FAILED (exit {result.exit_code}): {cmd}")
        tail = (result.stderr or result.stdout or "").strip().splitlines()[-4:]
        for line in tail:
            print(f"       {line}")
        return None
    return elapsed


def host_run(argv, timeout=300, cwd=None):
    """Time the same work natively. Returns None if the tool is missing/failed."""
    if shutil.which(argv[0]) is None:
        return None
    start = time.monotonic()
    try:
        proc = subprocess.run(
            argv, capture_output=True, timeout=timeout, cwd=cwd, text=True
        )
    except (subprocess.TimeoutExpired, OSError):
        return None
    elapsed = time.monotonic() - start
    if proc.returncode != 0:
        print(f"    (native {argv[0]} failed: exit {proc.returncode}, no ratio)")
        return None
    return elapsed


def median_guest(sandbox, cmd, repeats, timeout=300):
    times = [guest_run(sandbox, cmd, timeout) for _ in range(repeats)]
    times = [t for t in times if t is not None]
    return statistics.median(times) if times else None


def median_host(argv, repeats, timeout=300):
    times = [host_run(argv, timeout) for _ in range(repeats)]
    times = [t for t in times if t is not None]
    return statistics.median(times) if times else None


def main():
    print(f"snapshot: {os.environ['VPOD_SNAPSHOT']}\n")

    tmp = Path(tempfile.mkdtemp(prefix="vpod-bench-"))

    # ---------------------------------------------------------------- floor
    print("── floor (SDK overhead, no native equivalent) ───────────────────")

    # Sandbox.create() is lazy: the guest snapshot restore happens on the first
    # command, not in create(). And the first create() in a machine's life also
    # pays a ~2.6s Cranelift compile of the 23MB wasm into an 86MB .cwasm.
    # Attribute all three separately or the "cold start" number is a lie.
    from vpod._component import _cwasm_cache_path, _get_or_load_component, locate_wasm

    wasm = locate_wasm()
    cached = _cwasm_cache_path(wasm).exists()
    t = time.monotonic()
    _get_or_load_component(wasm)
    record(
        "floor",
        "engine + component load",
        time.monotonic() - t,
        note="cwasm cached" if cached else "COMPILING cwasm (first run)",
    )

    t = time.monotonic()
    sandbox = Sandbox.create()
    record("floor", "Sandbox.create()", time.monotonic() - t, note="lazy, no restore yet")

    t = time.monotonic()
    first = sandbox.commands.run("echo ready")
    if first.success:
        record(
            "floor",
            "first command (guest restore)",
            time.monotonic() - t,
            note="real cold start",
        )

    with sandbox:
        # Warm the prefork daemon and settle DNS/TLS before anything is timed.
        sandbox.commands.run("python3 -c pass")
        sandbox.commands.run("echo warmup")

        t = time.monotonic()
        sandbox.code.run("pass")
        record("floor", "code.run('pass') warm", time.monotonic() - t)

        e = guest_run(sandbox, "echo hi")
        if e:
            record("floor", "commands.run('echo hi')", e, note="fixed round-trip")

        e = guest_run(sandbox, "python3 -c pass")
        n = host_run(["python3", "-c", "pass"])
        if e:
            record("floor", "python3 -c pass", e, n)

        # ------------------------------------------------------------ apk
        print("\n── apk (network + extract + spawns; no host equivalent) ─────────")
        e = guest_run(sandbox, "apk update")
        if e:
            record("apk", "apk update", e, note="network")

        e = guest_run(sandbox, "apk add ripgrep")
        if e:
            record("apk", "apk add ripgrep", e, note="network")

        # ------------------------------------------------------------- uv
        print("\n── uv (syscall/FS heavy) ────────────────────────────────────────")
        e = guest_run(sandbox, "rm -rf /tmp/bench-venv && uv venv /tmp/bench-venv")
        n = host_run(["uv", "venv", str(tmp / "venv")])
        if e:
            record("uv", "uv venv", e, n)

        e = guest_run(
            sandbox,
            "rm -rf /tmp/bench-tgt && uv pip install --no-cache --target /tmp/bench-tgt six",
        )
        n = host_run(
            ["uv", "pip", "install", "--no-cache", "--target", str(tmp / "tgt"), "six"]
        )
        if e:
            record("uv", "uv pip install six", e, n, note="network")

        # -------------------------------------------------------- ripgrep
        print("\n── ripgrep (CPU + FS over an identical corpus) ──────────────────")
        # code.run takes raw python, so no shell quoting to get wrong.
        setup = sandbox.code.run(
            'root = "/tmp/bench-corpus"\n' + CORPUS, timeout=300
        )
        host_corpus = tmp / "corpus"
        subprocess.run(
            ["python3", "-c", f'root = {str(host_corpus)!r}\n' + CORPUS],
            capture_output=True,
        )
        if not setup.success:
            print(f"    (guest corpus generation failed: {setup.error} — skipping)")
        else:
            e = guest_run(sandbox, "rg -c NEEDLE_MARKER_XYZ /tmp/bench-corpus | wc -l")
            n = host_run(["rg", "-c", "NEEDLE_MARKER_XYZ", str(host_corpus)])
            if e:
                record("ripgrep", "rg over 120 files / 48K lines", e, n)

        # ----------------------------------------------------------- wget
        print(f"\n── wget (median of {NET_REPEATS}; network variance is large) ────────────")
        e = median_guest(sandbox, "wget -q -O /dev/null http://example.com", NET_REPEATS)
        n = median_host(["wget", "-q", "-O", "/dev/null", "http://example.com"], NET_REPEATS)
        if e:
            record("wget", "wget http", e, n, note="network")

        e = median_guest(sandbox, "wget -q -O /dev/null https://example.com", NET_REPEATS)
        n = median_host(["wget", "-q", "-O", "/dev/null", "https://example.com"], NET_REPEATS)
        if e:
            record("wget", "wget https (TLS proxy)", e, n, note="network")

    shutil.rmtree(tmp, ignore_errors=True)

    # --------------------------------------------------------------- table
    print("\n\n══ summary ══════════════════════════════════════════════════════")
    print(f"{'workload':<32} {'guest':>9} {'native':>9} {'ratio':>8}")
    print("─" * 62)
    ratios = []
    for group, label, guest_s, native_s, note in results:
        nat = f"{native_s:8.3f}s" if native_s else "        —"
        if native_s:
            r = guest_s / native_s
            ratios.append((label, r))
            ratio = f"{r:7.1f}x"
        else:
            ratio = "       —"
        print(f"{label:<32} {guest_s:8.3f}s {nat} {ratio}")

    if ratios:
        print("─" * 62)
        best = min(ratios, key=lambda x: x[1])
        worst = max(ratios, key=lambda x: x[1])
        print(f"spread: {best[1]:.1f}x ({best[0]}) .. {worst[1]:.1f}x ({worst[0]})")
        print("\nThe spread is the finding. Quote the table, not one multiplier.")


if __name__ == "__main__":
    main()
