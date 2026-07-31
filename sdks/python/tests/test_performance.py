"""Performance regression tests."""

import json
import os
import time
from pathlib import Path

import pytest

from vpod import Sandbox
from vpod.snapshots import pull

pytestmark = pytest.mark.performance

SHARED = json.loads(
    (Path(__file__).resolve().parents[2] / "performance-workloads.json").read_text()
)
WORKLOADS = SHARED["workloads"]
CEILINGS = SHARED["wallCeilings"]
RECORDED_SNAPSHOT = SHARED["recordedSnapshot"]

REPORT: dict = {"guest": {}, "wall": {}}


def guest_program(body: str) -> str:
    return f"import time\n_t0=time.time()\n{body}\nprint(f'{{time.time()-_t0:.6f}}')"


@pytest.fixture(scope="module")
def box():
    started_at = time.perf_counter()
    with Sandbox.create() as sandbox:
        REPORT["wall"]["bootSeconds"] = time.perf_counter() - started_at

        sandbox.code.run("print('warm')", timeout=120)

        yield sandbox

    REPORT["snapshot"] = RECORDED_SNAPSHOT
    output = os.environ.get("VPOD_PERF_OUTPUT")
    text = json.dumps(REPORT, indent=2)
    if output:
        Path(output).write_text(text + "\n")
    print("\n" + text)


@pytest.fixture(scope="module")
def strict() -> bool:
    try:
        return pull("alpine:latest").name == RECORDED_SNAPSHOT
    except Exception:
        return False


def test_boot_within_ceiling(box):
    seconds = REPORT["wall"]["bootSeconds"]
    assert seconds < CEILINGS["bootSeconds"], (
        f"boot took {seconds:.2f}s, ceiling {CEILINGS['bootSeconds']}s"
    )


@pytest.mark.parametrize("name", list(WORKLOADS))
def test_workload_does_not_drift(box, strict, name):
    workload = WORKLOADS[name]
    program = guest_program(workload["body"])
    box.code.run(program, timeout=300)

    started_at = time.perf_counter()
    result = box.code.run(program, timeout=300)
    wall_seconds = time.perf_counter() - started_at

    assert result.success, f"{name} failed in the guest: {result.error}"
    guest_seconds = float(result.text.strip())

    REPORT["guest"][name] = {
        "guestSeconds": guest_seconds,
        "wallSeconds": wall_seconds,
        "throughput": guest_seconds / wall_seconds,
        "expected": workload["guestSeconds"],
    }

    if not strict:
        pytest.skip("a different snapshot: numbers reported, not asserted")

    expected = workload["guestSeconds"]
    drift = abs(guest_seconds - expected) / expected
    assert drift <= workload["tolerance"], (
        f"{name} guest time is {guest_seconds:.6f}s, recorded {expected}s "
        f"(tolerance {workload['tolerance'] * 100}%). Guest time is deterministic, "
        f"so this is the guest doing a different amount of work, not the host "
        f"running slower. If the change is intended, update the constant in "
        f"sdks/performance-workloads.json -- and the TypeScript suite reads the "
        f"same file, so both move together."
    )


def test_throughput_above_floor(box):
    measured = REPORT["guest"]
    assert measured, "no workload ran, so there is nothing to divide"

    best = max(entry["throughput"] for entry in measured.values())
    REPORT["wall"]["throughput"] = best

    assert best > CEILINGS["throughputFloor"], (
        f"throughput is {best:.2f}x guest-seconds per wall-second, floor "
        f"{CEILINGS['throughputFloor']}x. Either the emulator regressed badly or "
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
