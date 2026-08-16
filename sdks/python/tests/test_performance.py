"""Performance regression tests."""

import hashlib
import json
import os
import time
from pathlib import Path

import pytest

from vpod import Sandbox
from vpod.snapshots import pull

pytestmark = pytest.mark.performance

SHARED_PATH = Path(__file__).resolve().parents[2] / "performance-workloads.json"
SHARED = json.loads(SHARED_PATH.read_text())
WORKLOADS = SHARED["workloads"]
CEILINGS = SHARED["wallCeilings"]

BASELINE_NAME = "perf.json"


def _baseline_url() -> str | None:
    explicit = os.environ.get("VPOD_PERF_BASELINE")
    if explicit:
        return explicit

    from vpod.snapshots import _resolve_registry_url

    registry = _resolve_registry_url(None)
    return registry.rsplit("/", 1)[0] + "/" + BASELINE_NAME


def _load_baseline() -> dict:
    """The channel's record of which bytes were measured and what they measured."""
    url = _baseline_url()
    if url is None:
        return {}
    try:
        if url.startswith(("http://", "https://")):
            import urllib.request

            with urllib.request.urlopen(url, timeout=30) as response:
                return json.loads(response.read())
        return json.loads(Path(url).read_text())
    except Exception as thrown:
        print(f"\nno baseline at {url}: {thrown}")
        return {}


def _entry_for(baseline: dict, digests: set[str]) -> dict | None:
    """Match on bytes, not on name: v1 and tests/v1 ship different builds under the
    same file name, and the two SDKs hash different encodings of the same image."""
    for entry in baseline.get("snapshots", {}).values():
        recorded = {value for value in entry.get("sha256", {}).values() if value}
        if recorded & digests:
            return entry
    return None


def _throughput_floor() -> tuple[str, float]:
    """ One floor per tier. The interpreter's floor is far below what a translated engine manages, so reusing it would pass even if AOT stopped dispatching entirely and every block fell back to the interpreter, which is the regression it exists to catch. """
    from vpod import _component

    tier = _component._active_tier or "unknown"
    if tier == "aot":
        return tier, CEILINGS["throughputFloorAot"]
    return tier, CEILINGS["throughputFloor"]

REPORT: dict = {"guest": {}, "wall": {}}

RECORDING = os.environ.get("VPOD_PERF_RECORD") == "1"


def _rerecord(previous: dict | None) -> None:
    """Write the baseline next to the bundle, to upload with the snapshots it describes."""
    digest = REPORT.get("snapshotSha256")
    measured = REPORT["guest"]
    if digest is None or not measured:
        print("\nnothing to record: the snapshot never resolved or no workload ran")
        return

    destination = Path(
        os.environ.get("VPOD_PERF_RECORD_TO")
        or Path(__file__).resolve().parents[3] / "dist" / "registry-bundle" / BASELINE_NAME
    )
    existing = json.loads(destination.read_text()) if destination.exists() else {}
    snapshots = existing.get("snapshots", {})

    snapshots[REPORT.get("snapshotId", "alpine-3.23.0-256mb")] = {
        "sha256": {
            "raw": digest,
            "lz4": REPORT.get("snapshotSha256Compressed"),
        },
        "guestSeconds": {
            name: round(entry["guestSeconds"], 6) for name, entry in measured.items()
        },
    }

    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps({"snapshots": snapshots}, indent=2) + "\n")

    print(f"\nrecorded {destination}")
    print(f"  raw {digest[:16]}  lz4 {(REPORT.get('snapshotSha256Compressed') or '-')[:16]}")
    for name, entry in measured.items():
        was = (previous or {}).get("guestSeconds", {}).get(name)
        now = entry["guestSeconds"]
        if was:
            print(f"  {name:<10} {was:.6f}s -> {now:.6f}s  ({(now - was) / was * 100:+.1f}%)")
        else:
            print(f"  {name:<10} {now:.6f}s  (no previous baseline)")
    print(f"Upload {BASELINE_NAME} to the channel alongside the snapshots.")


def guest_program(body: str) -> str:
    return f"import time\n_t0=time.time()\n{body}\nprint(f'{{time.time()-_t0:.6f}}')"


@pytest.fixture(scope="module")
def box():
    started_at = time.perf_counter()
    with Sandbox.create() as sandbox:
        REPORT["wall"]["bootSeconds"] = time.perf_counter() - started_at

        sandbox.code.run("print('warm')", timeout=120)

        yield sandbox

    output = os.environ.get("VPOD_PERF_OUTPUT")
    text = json.dumps(REPORT, indent=2)
    if output:
        Path(output).write_text(text + "\n")
    print("\n" + text)

    if RECORDING:
        _rerecord(BASELINE_ENTRY.get("entry"))


BASELINE_ENTRY: dict = {}


@pytest.fixture(scope="module")
def baseline() -> dict | None:
    """ Guest time is deterministic, so a baseline only means anything against the bytes it was measured from. A name does not establish that: v1 and tests/v1 ship different builds under the same file name, and comparing against the wrong one reports a regression in the emulator when the only thing that changed was the guest. So the channel publishes its timings keyed by digest, and we look ours up by the bytes we actually pulled. """
    try:
        pulled = pull("alpine:latest")
    except Exception:
        return None

    digest = hashlib.sha256(pulled.read_bytes()).hexdigest()
    REPORT["snapshotSha256"] = digest
    REPORT["snapshotId"] = pulled.stem
    digests = {digest}

    meta = pulled.with_suffix(".meta")
    if meta.exists():
        compressed = meta.read_text().strip()
        REPORT["snapshotSha256Compressed"] = compressed
        digests.add(compressed)

    if RECORDING:
        BASELINE_ENTRY["entry"] = _entry_for(_load_baseline(), digests)
        return None

    entry = _entry_for(_load_baseline(), digests)
    REPORT["strict"] = entry is not None

    if entry is None and os.environ.get("VPOD_PERF_REQUIRE_STRICT") == "1":
        pytest.fail(
            f"the snapshot is {digest[:16]}, and {_baseline_url()} has no timings for "
            f"those bytes. Either the channel was republished without re-recording, or "
            f"this is a different image. Re-record with VPOD_PERF_RECORD=1 and upload "
            f"{BASELINE_NAME} alongside the snapshots."
        )
    return entry


def test_boot_within_ceiling(box):
    seconds = REPORT["wall"]["bootSeconds"]
    assert seconds < CEILINGS["bootSeconds"], (
        f"boot took {seconds:.2f}s, ceiling {CEILINGS['bootSeconds']}s"
    )


@pytest.mark.parametrize("name", list(WORKLOADS))
def test_workload_does_not_drift(box, baseline, name):
    workload = WORKLOADS[name]
    program = guest_program(workload["body"])
    box.code.run(program, timeout=300)

    started_at = time.perf_counter()
    result = box.code.run(program, timeout=300)
    wall_seconds = time.perf_counter() - started_at

    assert result.success, f"{name} failed in the guest: {result.error}"
    guest_seconds = float(result.text.strip())

    expected = (baseline or {}).get("guestSeconds", {}).get(name)
    REPORT["guest"][name] = {
        "guestSeconds": guest_seconds,
        "wallSeconds": wall_seconds,
        "throughput": guest_seconds / wall_seconds,
        "expected": expected,
    }

    if expected is None:
        pytest.skip("no published baseline for these bytes: numbers reported, not asserted")

    drift = abs(guest_seconds - expected) / expected
    assert drift <= workload["tolerance"], (
        f"{name} guest time is {guest_seconds:.6f}s, the channel recorded {expected}s "
        f"for these exact bytes (tolerance {workload['tolerance'] * 100}%). Guest time "
        f"is deterministic, so this is the guest doing a different amount of work, not "
        f"the host running slower. If the change is intended, re-record with "
        f"VPOD_PERF_RECORD=1 and republish {BASELINE_NAME}."
    )


def test_throughput_above_floor(box):
    measured = REPORT["guest"]
    assert measured, "no workload ran, so there is nothing to divide"

    best = max(entry["throughput"] for entry in measured.values())
    REPORT["wall"]["throughput"] = best

    tier, floor = _throughput_floor()
    REPORT["wall"]["tier"] = tier
    REPORT["wall"]["throughputFloor"] = floor

    assert best > floor, (
        f"throughput is {best:.2f}x guest-seconds per wall-second, floor "
        f"{floor}x for the {tier} tier. Either the emulator regressed badly or "
        f"this runner is far slower than any seen before."
    )


def test_command_overhead(box):
    rounds = 10
    started_at = time.perf_counter()
    for _ in range(rounds):
        box.commands.run("echo x", timeout=60)
    per_call = (time.perf_counter() - started_at) / rounds
    REPORT["wall"]["shellPerCallSeconds"] = per_call

    assert per_call < CEILINGS["shellPerCallSeconds"], (
        f"a trivial command costs {per_call:.3f}s, ceiling "
        f"{CEILINGS['shellPerCallSeconds']}s"
    )


def test_network_round_trip(box):
    probe = box.commands.run(
        "wget -q -O- https://pypi.org/pypi/six/json 2>&1 | head -c 20", timeout=60
    )
    if probe.exit_code != 0:
        pytest.skip("no route to the network")

    started_at = time.perf_counter()
    result = box.commands.run(
        "wget -q -O- https://pypi.org/pypi/six/json 2>&1 | head -c 20", timeout=120
    )
    seconds = time.perf_counter() - started_at
    REPORT["wall"]["networkRoundTripSeconds"] = seconds

    assert result.exit_code == 0, result.stderr
    assert seconds < CEILINGS["networkRoundTripSeconds"], (
        f"a round trip took {seconds:.2f}s, ceiling "
        f"{CEILINGS['networkRoundTripSeconds']}s"
    )
