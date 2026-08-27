import queue

import pytest

from vpod import Sandbox


def test_start_hands_back_a_handle_that_has_not_run_yet(mock_component):
    with Sandbox.create() as sbx:
        execution = sbx.commands.start("echo hi")

        assert not execution.done
        assert execution.exit_code is None

        execution.wait()
        assert execution.done


def test_the_caller_drives_the_command_one_slice_at_a_time(mock_component):
    with Sandbox.create() as sbx:
        execution = sbx.commands.start("echo hi")

        # Nothing has moved until step() is called: the consumer sets the pace.
        assert execution.stdout == ""
        execution.step()
        assert execution.done


def test_result_refuses_to_answer_before_the_command_ends(mock_component):
    with Sandbox.create() as sbx:
        execution = sbx.commands.start("echo hi")

        with pytest.raises(RuntimeError, match="still running"):
            execution.result()


def test_writes_reach_the_guest_as_bytes(mock_component):
    with Sandbox.create() as sbx:
        execution = sbx.commands.start("cat", tty=True)
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
        execution = sbx.commands.start("cat", stdin_open=True)
        execution.write("early")
        execution.wait()

    assert order[0] == "stdin", f"the staging window was missed: {order}"
    assert b"".join(data for _, data in mock_component["stdin_writes"]) == b"early"


def test_nothing_is_sent_when_there_is_no_input(mock_component):
    with Sandbox.create() as sbx:
        sbx.commands.run("echo hi")

    assert mock_component["stdin_writes"] == []


def test_stdin_is_sent_verbatim_with_no_control_bytes(mock_component):
    for payload in ("data\n", "no trailing newline"):
        mock_component["stdin_writes"].clear()
        with Sandbox.create() as sbx:
            sbx.commands.run("cat", stdin=payload)

        sent = b"".join(data for _, data in mock_component["stdin_writes"])
        assert sent == payload.encode(), payload
        assert b"\x04" not in sent, "a stray Ctrl-D can kill the session"


def test_streaming_stdin_without_a_tty_is_refused(mock_component):
    inbox = queue.Queue()
    inbox.put("first\n")

    with Sandbox.create() as sbx:
        with pytest.raises(ValueError, match="tty=True"):
            sbx.commands.run("cat", stdin=inbox)


def test_streaming_stdin_is_accepted_on_a_tty(mock_component):
    inbox = queue.Queue()
    inbox.put("first\n")

    with Sandbox.create() as sbx:
        sbx.commands.run("cat", stdin=inbox, tty=True, timeout=0)

    sent = b"".join(data for _, data in mock_component["stdin_writes"])
    assert sent == b"first\n"
    assert b"\x04" not in sent


def test_an_unsupported_stdin_is_refused_rather_than_ignored(mock_component):
    with Sandbox.create() as sbx:
        with pytest.raises(TypeError, match="queue.Queue"):
            sbx.commands.run("cat", stdin=object())


def _spy_on_slices(mock_component, seen):
    real = mock_component["exports"]["session-exec-slice"]

    def spy(sid, command, timeout=None, slice_nanos=0, mode="closed"):
        seen["mode"] = mode
        seen["timeout"] = timeout
        return real(sid, command, timeout, slice_nanos, mode)

    mock_component["exports"]["session-exec-slice"] = spy


def test_the_mode_follows_from_what_the_caller_asked_for(mock_component):
    seen = {}
    _spy_on_slices(mock_component, seen)

    with Sandbox.create() as sbx:
        sbx.commands.run("echo hi")
        assert seen["mode"] == "closed", "stdin stays closed unless asked for"

        sbx.commands.run("cat", stdin="x\n")
        assert seen["mode"] == "piped", "finite input needs canonical mode for Ctrl-D"

        sbx.commands.run("python3", tty=True, timeout=0)
        assert seen["mode"] == "terminal"

        assert seen["timeout"] == 0
