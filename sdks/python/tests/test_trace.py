import json
import threading
from dataclasses import asdict
from pathlib import Path

import pytest

from vpod import Sandbox, Trace
from vpod.trace import NOT_ENABLED, NOT_SUPPORTED, trace_options

FIXTURES = json.loads(
    (Path(__file__).resolve().parents[2] / "trace-summaries.json").read_text()
)["cases"]


@pytest.mark.parametrize("case", FIXTURES, ids=[case["name"] for case in FIXTURES])
def test_summaries_match_the_shared_fixture(case):
    trace = Trace(case["events"])

    assert [asdict(entry) for entry in trace.files()] == case["files"]
    assert [asdict(entry) for entry in trace.files(internal=True, noise=True)] == case[
        "files_including_internal_and_noise"
    ]
    assert [asdict(entry) for entry in trace.network()] == case["network"]
    assert [asdict(entry) for entry in trace.network(internal=True)] == case[
        "network_including_internal"
    ]
    assert [asdict(entry) for entry in trace.processes()] == case["processes"]
    assert [asdict(entry) for entry in trace.processes(internal=True)] == case[
        "processes_including_internal"
    ]
    assert trace.complete is case["complete"]


@pytest.mark.parametrize("case", FIXTURES, ids=[case["name"] for case in FIXTURES])
def test_json_lines_round_trip_every_event(case):
    lines = Trace(case["events"]).to_jsonl().splitlines()
    assert [json.loads(line) for line in lines] == case["events"]


def test_a_trace_is_a_copy_the_caller_cannot_change():
    events = [{"v": 1, "seq": 0, "guest_ns": 0, "wall_ms": 0, "kind": "trace.dropped", "count": 1}]
    trace = Trace(events)
    events.clear()
    trace.events.clear()
    assert len(trace.events) == 1


def test_true_turns_every_source_on():
    assert trace_options(True) == {
        "processes": True,
        "files": True,
        "network": True,
        "mounts": True,
        "buffer_bytes": 0,
    }


def test_a_dict_turns_on_only_the_sources_it_names():
    assert trace_options({"network": True}) == {
        "processes": False,
        "files": False,
        "network": True,
        "mounts": False,
        "buffer_bytes": 0,
    }


def test_an_unknown_source_is_refused():
    with pytest.raises(ValueError, match="netwrok"):
        trace_options({"netwrok": True})


def test_each_command_carries_only_its_own_trace(mock_component):
    with Sandbox.create(trace=True) as sbx:
        first = sbx.commands.run("echo one")
        second = sbx.commands.run("echo two")

        assert [node.argv for node in first.trace.processes()] == [["sh", "-c", "echo one"]]
        assert [node.argv for node in second.trace.processes()] == [["sh", "-c", "echo two"]]
        assert [node.argv[2] for node in sbx.trace.collect().processes()] == [
            "echo one",
            "echo two",
        ]


def test_the_trace_starts_with_the_sources_asked_for(mock_component):
    with Sandbox.create(trace={"files": True, "buffer_bytes": 4096}) as sbx:
        sbx.commands.run("true")
        (session,) = mock_component["traced_sessions"].values()
        options = session["options"]

        assert (options.processes, options.files, options.network, options.mounts) == (
            False,
            True,
            False,
            False,
        )
        assert getattr(options, "buffer-bytes") == 4096


def test_the_whole_trace_survives_closing_the_sandbox(mock_component):
    sbx = Sandbox.create(trace=True)
    with sbx:
        sbx.commands.run("echo kept")

    assert [node.argv[2] for node in sbx.trace.collect().processes()] == ["echo kept"]


def test_clear_forgets_what_was_recorded(mock_component):
    with Sandbox.create(trace=True) as sbx:
        sbx.commands.run("echo before")
        sbx.trace.clear()
        sbx.commands.run("echo after")

        assert [node.argv[2] for node in sbx.trace.collect().processes()] == ["echo after"]


def test_watch_follows_commands_as_they_run_and_ends_when_the_sandbox_closes(mock_component):
    sbx = Sandbox.create(trace=True)
    seen = []
    with sbx:
        events = sbx.trace.watch()
        follower = threading.Thread(target=lambda: seen.extend(events))
        follower.start()
        sbx.commands.run("echo watched")
    follower.join(timeout=5)

    assert not follower.is_alive()
    assert [event["argv"][2] for event in seen] == ["echo watched"]


def test_without_tracing_every_trace_says_how_to_turn_it_on(mock_component):
    with Sandbox.create() as sbx:
        result = sbx.commands.run("echo hi")

        with pytest.raises(RuntimeError, match="trace=True"):
            result.trace
        with pytest.raises(RuntimeError, match="trace=True"):
            sbx.trace.collect()
        with pytest.raises(RuntimeError, match="trace=True"):
            sbx.trace.watch()

    assert "trace=True" in NOT_ENABLED


def test_an_engine_without_trace_support_is_refused_up_front(mock_component):
    for name in ("session-trace-start", "session-trace-drain", "session-trace-stop"):
        del mock_component["exports"][name]

    with pytest.raises(RuntimeError, match="engine=\"default\""):
        Sandbox.create(trace=True)
    assert "engine=\"default\"" in NOT_SUPPORTED

    with Sandbox.create() as sbx:
        assert sbx.commands.run("echo untraced").success
