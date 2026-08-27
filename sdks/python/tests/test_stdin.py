import queue

import pytest

from vpod import Sandbox


def _hold_open(mock_component, slices: int):
    real = mock_component["exports"]["session-exec-slice"]
    left = {"n": slices}

    def spy(sid, command, timeout=None, slice_nanos=0, mode="closed"):
        if command is None:
            result = real(sid, "true", timeout, slice_nanos, mode)
        else:
            result = real(sid, command, timeout, slice_nanos, mode)
        if left["n"] > 0:
            left["n"] -= 1
            setattr(result.payload, "exit-code", None)
        return result

    mock_component["exports"]["session-exec-slice"] = spy


def _sent(mock_component) -> bytes:
    return b"".join(data for _, data in mock_component["stdin_writes"])


def test_start_hands_back_a_handle_that_has_not_run_yet(mock_component):
    with Sandbox.create() as sbx:
        execution = sbx.commands._start("echo hi", 120, "closed")

        assert not execution.done
        assert execution.exit_code is None

        execution.wait()
        assert execution.done


def test_the_caller_drives_the_command_one_slice_at_a_time(mock_component):
    with Sandbox.create() as sbx:
        execution = sbx.commands._start("echo hi", 120, "closed")

        # Nothing has moved until step() is called: the consumer sets the pace.
        assert execution.stdout == ""
        execution.step()
        assert execution.done


def test_result_refuses_to_answer_before_the_command_ends(mock_component):
    with Sandbox.create() as sbx:
        execution = sbx.commands._start("echo hi", 120, "closed")

        with pytest.raises(RuntimeError, match="still running"):
            execution.result()


def test_writes_reach_the_guest_as_bytes(mock_component):
    with Sandbox.create() as sbx:
        execution = sbx.commands._start("cat", 120, "terminal")
        execution.write("hello\n")
        execution.step()

    writes = mock_component["stdin_writes"]
    assert len(writes) == 1
    assert writes[0][1] == b"hello\n"


def test_input_is_never_sent_before_the_command_is_in_flight(mock_component):
    order = []
    real = mock_component["exports"]["session-exec-slice"]

    def slice_spy(sid, command, timeout=None, slice_nanos=0, mode="closed"):
        order.append("slice")
        return real(sid, command, timeout, slice_nanos, mode)

    real_stdin = mock_component["exports"]["session-stdin"]

    def stdin_spy(sid, data):
        order.append("stdin")
        return real_stdin(sid, data)

    mock_component["exports"]["session-exec-slice"] = slice_spy
    mock_component["exports"]["session-stdin"] = stdin_spy

    with Sandbox.create() as sbx:
        execution = sbx.commands._start("cat", 120, "piped")
        execution.write("early")
        execution.wait()

    assert order[0] == "stdin", f"the staging window was missed: {order}"
    assert _sent(mock_component) == b"early"


def test_nothing_is_sent_when_there_is_no_input(mock_component):
    with Sandbox.create() as sbx:
        sbx.commands.run("echo hi")

    assert mock_component["stdin_writes"] == []


def test_stdin_is_sent_verbatim_with_no_control_bytes(mock_component):
    for payload in ("data\n", "no trailing newline"):
        mock_component["stdin_writes"].clear()
        with Sandbox.create() as sbx:
            sbx.commands.run("cat", stdin=payload)

        assert _sent(mock_component) == payload.encode(), payload
        assert b"\x04" not in _sent(mock_component), "a stray Ctrl-D can kill the session"


def test_a_queue_streams_without_the_caller_asking_for_a_tty(mock_component):
    inbox = queue.Queue()
    inbox.put("first\n")

    with Sandbox.create() as sbx:
        sbx.commands.run("cat", stdin=inbox, timeout=0)

    assert _sent(mock_component) == b"first\n"
    assert b"\x04" not in _sent(mock_component), "an open queue is not end-of-input"


def test_closing_a_queue_sends_end_of_file(mock_component):
    inbox = queue.Queue()
    inbox.put("first\n")
    inbox.put(None)

    with Sandbox.create() as sbx:
        sbx.commands.run("cat", stdin=inbox, timeout=0)

    assert _sent(mock_component) == b"first\n\x04"


def test_an_iterable_feeds_one_chunk_per_slice_then_ends_the_input(mock_component):
    _hold_open(mock_component, slices=3)

    with Sandbox.create() as sbx:
        sbx.commands.run("cat", stdin=iter(["a\n", b"b\n"]), timeout=0)

    chunks = [data for _, data in mock_component["stdin_writes"]]
    assert chunks == [b"a\n", b"b\n", b"\x04"]


def test_end_of_file_is_sent_once_even_across_later_slices(mock_component):
    _hold_open(mock_component, slices=4)

    with Sandbox.create() as sbx:
        sbx.commands.run("cat", stdin=iter(["only\n"]), timeout=0)

    assert _sent(mock_component).count(b"\x04") == 1


def test_a_generator_that_produces_nothing_still_ends_the_input(mock_component):
    with Sandbox.create() as sbx:
        sbx.commands.run("cat", stdin=iter([]), timeout=0)

    assert _sent(mock_component) == b"\x04"


def test_an_unsupported_stdin_is_refused_rather_than_ignored(mock_component):
    with Sandbox.create() as sbx:
        with pytest.raises(TypeError):
            sbx.commands.run("cat", stdin=object())


def _spy_on_slices(mock_component, seen):
    real = mock_component["exports"]["session-exec-slice"]

    def spy(sid, command, timeout=None, slice_nanos=0, mode="closed"):
        seen["mode"] = mode
        seen["timeout"] = timeout
        return real(sid, command, timeout, slice_nanos, mode)

    mock_component["exports"]["session-exec-slice"] = spy


def test_the_mode_follows_from_the_kind_of_stdin(mock_component):
    seen = {}
    _spy_on_slices(mock_component, seen)

    with Sandbox.create() as sbx:
        sbx.commands.run("echo hi")
        assert seen["mode"] == "closed", "stdin stays closed unless asked for"

        sbx.commands.run("cat", stdin="x\n")
        assert seen["mode"] == "piped", "finite input is staged to a file"

        sbx.commands.run("cat", stdin=iter(["x\n"]), timeout=0)
        assert seen["mode"] == "terminal", "a stream cannot be staged, so it needs a tty"

        sbx.commands.run("python3", tty=True, timeout=0)
        assert seen["mode"] == "terminal"

        assert seen["timeout"] == 0
