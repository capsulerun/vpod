import os
import re
import shutil
from pathlib import Path

import pytest
from vpod import Sandbox, snapshots

pytestmark = pytest.mark.integration

# Random name that says nothing about memory to test
ANONYMOUS = "snap_a422ba54177ff2ee.snap"

SMALLEST_PLAUSIBLE_SHARE = 0.85
SNAPSHOTS = ["vsnap-base-256mb", "vsnap-base-512mb", "vsnap-base-1024mb"]


def captured_megabytes(file_name):
    match = re.search(r"(\d+)mb", file_name, re.IGNORECASE)
    return None if match is None else int(match.group(1))


def guest_memory_total_mb(path, monkeypatch):
    monkeypatch.setenv("VPOD_SNAPSHOT", str(path))
    with Sandbox.create() as sandbox:
        result = sandbox.commands.run("free -m | awk '/^Mem:/ { print $2 }'")
        assert result.success, result.stderr
        total = result.stdout.strip()
        assert total.isdigit(), f"free -m printed {total!r}"
        return int(total)


@pytest.fixture(autouse=True)
def one_engine_at_a_time():
    from vpod import _component

    yield
    _component._instance_cache.clear()


@pytest.fixture
def under_another_name(tmp_path):

    def link(source):
        destination = tmp_path / ANONYMOUS
        try:
            os.link(source, destination)
        except OSError:
            shutil.copy2(source, destination)
        return destination

    return link


@pytest.mark.parametrize("name", SNAPSHOTS)
def test_size_comes_from_the_snapshot_not_the_file_name(
    name, monkeypatch, under_another_name
):
    monkeypatch.delenv("VPOD_SNAPSHOT", raising=False)
    source = Path(snapshots.pull(name))

    named = guest_memory_total_mb(source, monkeypatch)
    anonymous = guest_memory_total_mb(under_another_name(source), monkeypatch)

    assert anonymous == named, (
        f"mounted as {source.name} the guest saw {named} MB, but the same bytes "
        f"mounted as {ANONYMOUS} saw {anonymous} MB. The size is being read off "
        f"the file name instead of the snapshot header, so any snapshot not "
        f"named after its size gets the wrong machine."
    )


@pytest.mark.parametrize("name", SNAPSHOTS)
def test_guest_gets_the_size_it_was_captured_at(name, monkeypatch):
    monkeypatch.delenv("VPOD_SNAPSHOT", raising=False)
    source = Path(snapshots.pull(name))

    captured = captured_megabytes(name)
    assert captured is not None, f"{name} does not say what size it is"

    total = guest_memory_total_mb(source, monkeypatch)
    assert captured * SMALLEST_PLAUSIBLE_SHARE <= total <= captured, (
        f"a {captured} MB snapshot restored to a guest that sees {total} MB"
    )
