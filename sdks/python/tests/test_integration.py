import queue
import socket
import threading
import time

from vpod.snapshots import catalog
import pytest
from vpod import Sandbox, snapshots

pytestmark = pytest.mark.integration


def test_stateless_command():
    with Sandbox.create() as sbx:
        result = sbx.commands.run("echo hello")
        assert result.success
        assert "hello" in result.stdout


def test_stateless_exit_code():
    sbx = Sandbox.create()
    result = sbx.commands.run("false")
    assert not result.success
    assert result.exit_code == 1


def test_session_env_persists():
    with Sandbox.create() as sbx:
        sbx.commands.run("export FOO=bar")
        result = sbx.commands.run("echo $FOO")
        assert "bar" in result.stdout


def test_session_filesystem_persists():
    with Sandbox.create() as sbx:
        sbx.commands.run("touch /tmp/hello.txt")
        result = sbx.commands.run("ls /tmp")
        assert "hello.txt" in result.stdout


def test_session_code_python():
    with Sandbox.create() as sbx:
        result = sbx.code.run("print(2 + 2)")
        assert result.success
        assert "4" in result.text


def test_session_code_python_persistent():
    with Sandbox.create() as sbx:
        sbx.code.run("x = 1")
        sbx.code.run("y = 1")
        result = sbx.code.run("print(x + y)")

        assert result.success
        assert "2" in result.text


def test_session_code_error():
    with Sandbox.create() as sbx:
        result = sbx.code.run("1 / 0")
        assert not result.success
        assert result.error is not None


def test_session_code_printed_error_word_is_not_a_failure():
    with Sandbox.create() as sbx:
        result = sbx.code.run(
            "print('Error handling is hard')\n"
            "print('config not found, using defaults')"
        )
        assert result.success, result.error
        assert result.error is None


def test_session_code_silent_nonzero_exit_is_a_failure():
    with Sandbox.create() as sbx:
        result = sbx.code.run("import sys\nsys.exit(3)")
        assert not result.success
        assert "3" in result.error


def test_session_code_output_is_not_quoted_as_the_failure_reason():
    with Sandbox.create() as sbx:
        result = sbx.code.run("import sys\nprint('saving to disk')\nsys.exit(3)")
        assert not result.success
        assert result.error == "exited 3"
        assert result.logs == ["saving to disk"]


def test_session_code_quotes_the_exception_when_one_was_raised():
    with Sandbox.create() as sbx:
        result = sbx.code.run("print('before')\nraise ValueError('boom')")
        assert not result.success
        assert result.error == "ValueError: boom"


def test_session_code_stderr_is_reachable_on_success():
    with Sandbox.create() as sbx:
        result = sbx.code.run("import sys\nprint('out')\nsys.stderr.write('careful\\n')")
        assert result.success
        assert "out" in result.logs
        assert "careful" in result.stderr or "careful" in result.text


def test_session_code_text_has_no_carriage_returns():
    with Sandbox.create() as sbx:
        result = sbx.code.run("print('a')\nprint('b')")
        assert result.text == "a\nb"
        assert sbx.commands.run("printf 'x\\ny\\n'").stdout == "x\ny"


def test_code_requires_session():
    sbx = Sandbox.create()
    with pytest.raises(RuntimeError, match="requires a session"):
        sbx.code.run("print(1)")


def test_multiline_shell_script():
    with Sandbox.create() as sbx:
        result = sbx.commands.run(
            "for i in 1 2 3; do echo $i; done"
        )
        assert result.success
        assert "1" in result.stdout
        assert "2" in result.stdout
        assert "3" in result.stdout


def test_python_imports_persist():
    with Sandbox.create() as sbx:
        sbx.code.run("import json")
        sbx.code.run("data = {'key': 'value'}")
        result = sbx.code.run("print(json.dumps(data))")
        assert result.success
        assert "key" in result.text
        assert "value" in result.text


def test_python_list_comprehension():
    with Sandbox.create() as sbx:
        result = sbx.code.run("print([x**2 for x in range(5)])")
        assert result.success
        assert "[0, 1, 4, 9, 16]" in result.text


def test_python_multiline_function():
    with Sandbox.create() as sbx:
        sbx.code.run("def add(a, b):\n    return a + b")
        result = sbx.code.run("print(add(10, 20))")
        assert result.success
        assert "30" in result.text


def test_shell_pipe_and_grep():
    with Sandbox.create() as sbx:
        sbx.commands.run("echo -e 'apple\\nbanana\\ncherry' > /tmp/fruits.txt")
        result = sbx.commands.run("cat /tmp/fruits.txt | grep banana")
        assert result.success
        assert "banana" in result.stdout


def test_python_exception_handling():
    with Sandbox.create() as sbx:
        sbx.code.run(
            "def safe_divide(a, b):\n"
            "    try:\n"
            "        return a / b\n"
            "    except ZeroDivisionError:\n"
            "        return 'error'"
        )
        result = sbx.code.run("print(safe_divide(10, 0))")
        assert result.success
        assert "error" in result.text


def test_concurrent_file_operations():
    with Sandbox.create() as sbx:
        sbx.commands.run("mkdir -p /tmp/test")
        sbx.commands.run("touch /tmp/test/file1.txt /tmp/test/file2.txt /tmp/test/file3.txt")
        result = sbx.commands.run("ls /tmp/test | wc -l")
        assert result.success
        assert "3" in result.stdout


def test_python_data_structures():
    with Sandbox.create() as sbx:
        sbx.code.run("data = {'users': [{'id': 1, 'name': 'Alice'}, {'id': 2, 'name': 'Bob'}]}")
        result = sbx.code.run("print(len(data['users']))")
        assert result.success
        assert "2" in result.text


def test_shell_environment_isolation():
    sbx1 = Sandbox.create()
    sbx2 = Sandbox.create()

    result1 = sbx1.commands.run("echo test1")
    result2 = sbx2.commands.run("echo test2")

    assert result1.success
    assert result2.success
    assert "test1" in result1.stdout
    assert "test2" in result2.stdout


def test_python_string_operations():
    with Sandbox.create() as sbx:
        sbx.code.run("text = 'hello world'")
        result = sbx.code.run("print(text.upper().replace('WORLD', 'PYTHON'))")
        assert result.success
        assert "HELLO PYTHON" in result.text


def test_shell_subshell_exit_code():
    with Sandbox.create() as sbx:
        result = sbx.commands.run("(exit 42); echo $?")
        assert result.success
        assert "42" in result.stdout


# --- exit code tests ---

def test_stateless_exit_code_nonzero():
    sbx = Sandbox.create()
    result = sbx.commands.run("exit 42")
    assert result.exit_code == 42
    assert not result.success


def test_stateless_exit_code_zero():
    sbx = Sandbox.create()
    result = sbx.commands.run("true")
    assert result.exit_code == 0
    assert result.success


def test_session_exit_code_nonzero():
    with Sandbox.create() as sbx:
        result = sbx.commands.run("exit 7")
        assert result.exit_code == 7
        assert not result.success


def test_session_exit_code_zero():
    with Sandbox.create() as sbx:
        result = sbx.commands.run("true")
        assert result.exit_code == 0
        assert result.success


def test_session_exit_code_command_not_found():
    with Sandbox.create() as sbx:
        result = sbx.commands.run("notacommand_xyz")
        assert result.exit_code != 0
        assert not result.success


# timeout tests

def test_timeout_returns_124():
    import time
    with Sandbox.create() as sbx:
        start = time.time()
        result = sbx.commands.run("sleep 30", timeout=3)
        elapsed = time.time() - start

        assert result.exit_code == 124
        assert not result.success
        assert elapsed < 20, f"took {elapsed:.1f}s"


def test_within_timeout_succeeds():
    with Sandbox.create() as sbx:
        result = sbx.commands.run("sleep 1; echo done", timeout=30)
        assert result.success
        assert "done" in result.stdout


def test_session_survives_after_timeout():
    with Sandbox.create() as sbx:
        timed_out = sbx.commands.run("sleep 30", timeout=3)
        assert timed_out.exit_code == 124

        result = sbx.commands.run("echo alive")
        assert result.success
        assert "alive" in result.stdout


def test_code_timeout_returns_124():
    with Sandbox.create() as sbx:
        result = sbx.code.run("import time; time.sleep(30)", timeout=3)
        assert not result.success


def test_session_survives_after_code_timeout():
    with Sandbox.create() as sbx:
        timed_out = sbx.code.run("import time; time.sleep(30)", timeout=3)
        assert not timed_out.success

        result = sbx.code.run("print(6 * 7)")
        assert result.success
        assert "42" in result.text

# --- stderr tests ---

def test_stateless_stderr_captured():
    sbx = Sandbox.create()
    result = sbx.commands.run("echo error_msg >&2")
    assert "error_msg" in result.stderr
    assert result.stdout == ""


def test_stateless_stderr_not_in_stdout():
    sbx = Sandbox.create()
    result = sbx.commands.run("echo out_msg; echo err_msg >&2")
    assert "out_msg" in result.stdout
    assert "err_msg" in result.stderr
    assert "err_msg" not in result.stdout
    assert "out_msg" not in result.stderr


def test_session_stderr_captured():
    with Sandbox.create() as sbx:
        result = sbx.commands.run("echo session_error >&2")
        assert "session_error" in result.stderr
        assert result.stdout == ""


def test_session_stderr_not_in_stdout():
    with Sandbox.create() as sbx:
        result = sbx.commands.run("echo out; echo err >&2")
        assert "out" in result.stdout
        assert "err" in result.stderr
        assert "err" not in result.stdout
        assert "out" not in result.stderr


def test_stateless_stderr_with_exit_code():
    sbx = Sandbox.create()
    result = sbx.commands.run("echo fail >&2; exit 2")

    assert result.exit_code == 2
    assert "fail" in result.stderr
    assert result.stdout == ""


def test_session_stderr_with_exit_code():
    with Sandbox.create() as sbx:
        result = sbx.commands.run("echo fail >&2; exit 3")
        assert result.exit_code == 3
        assert "fail" in result.stderr


def test_python_arithmetic_precision():
    with Sandbox.create() as sbx:
        result = sbx.code.run("print(0.1 + 0.2)")
        assert result.success
        assert "0.3" in result.text


def test_shell_working_directory():
    with Sandbox.create() as sbx:
        sbx.commands.run("mkdir -p /tmp/workdir")
        sbx.commands.run("cd /tmp/workdir && touch created_here.txt")
        result = sbx.commands.run("ls /tmp/workdir")
        assert result.success
        assert "created_here.txt" in result.stdout

NETWORK_HOST = "kfuckkfmkyxe0l-tests.vpod.sh"
NETWORK_TIMEOUT = 15
NETWORK_ATTEMPTS = 2


def _wget(sbx, url, extra="", spider=True):
    mode = "--spider" if spider else "-O-"
    for attempt in range(NETWORK_ATTEMPTS):
        result = sbx.commands.run(
            f"wget -q {mode} -T {NETWORK_TIMEOUT} -t 1 {extra} {url}",
            timeout=NETWORK_TIMEOUT * 2,
        )
        if result.success or attempt == NETWORK_ATTEMPTS - 1:
            return result
        time.sleep(1)
    raise AssertionError("unreachable")


def _which_layer_failed(sbx):
    try:
        address = socket.gethostbyname(NETWORK_HOST)
    except OSError as unresolvable:
        return (
            f"the TEST RUNNER itself cannot resolve {NETWORK_HOST} ({unresolvable}), "
            "so this says nothing about the guest"
        )


    by_name_http = _wget(sbx, f"http://{NETWORK_HOST}")
    by_address = _wget(sbx, f"http://{address}", extra=f"--header 'Host: {NETWORK_HOST}'")

    resolv = sbx.commands.run("cat /etc/resolv.conf").stdout.strip().replace("\n", " ")
    link = sbx.commands.run("ip addr show eth0 2>&1 | head -1").stdout.strip()
    where = f"resolv.conf={resolv!r} eth0={link!r}"

    if by_name_http.success:
        return f"DNS and egress are both fine over http, so this is TLS or https-specific. {where}"
    if by_address.success:
        return (
            f"GUEST DNS is broken: {address} answers with a Host header, but the "
            f"name does not resolve. {where}"
        )

    # The guest cannot get out. Whether that is our problem depends entirely on
    # whether the machine running these tests can, so ask it the same question.
    # Without this the message cannot tell "vpod broke" from "this network
    # cannot reach the host", and those need completely different people.
    host_reachable = _host_can_reach(address, 80)
    blame = (
        "the RUNNER reaches it fine, so this is vpod's TCP path"
        if host_reachable
        else "the RUNNER cannot reach it either, so this is the network, not vpod"
    )
    return f"GUEST EGRESS is broken: {address}:80 unreachable without DNS. {blame}. {where}"


def _host_can_reach(address, port, timeout=10):
    """Can the machine running the tests open a TCP connection to the same place?"""
    try:
        with socket.create_connection((address, port), timeout=timeout):
            return True
    except OSError:
        return False


def test_network_dns_resolves():
    with Sandbox.create() as sbx:
        result = _wget(sbx, f"https://{NETWORK_HOST}")
        assert result.success, (
            f"exit={result.exit_code} stderr={result.stderr} :: {_which_layer_failed(sbx)}"
        )


def test_network_https_fetches_body():
    with Sandbox.create() as sbx:
        result = _wget(sbx, f"https://{NETWORK_HOST}", spider=False)
        assert result.success, (
            f"exit={result.exit_code} stderr={result.stderr} :: {_which_layer_failed(sbx)}"
        )
        assert "VPOD_TEST_OK" in result.stdout, f"stdout={result.stdout!r}"

def test_shared_vm_shell_writes_python_reads():
    with Sandbox.create() as sbx:
        sbx.commands.run("echo 'shared_value' > /tmp/shared.txt")
        result = sbx.code.run("print(open('/tmp/shared.txt').read().strip())")
        assert result.success
        assert "shared_value" in result.text


def test_shared_vm_python_writes_shell_reads():
    with Sandbox.create() as sbx:
        sbx.code.run("f = open('/tmp/py_shared.txt', 'w'); f.write('from_python\\n'); f.close()")
        result = sbx.commands.run("cat /tmp/py_shared.txt")
        assert result.success
        assert "from_python" in result.stdout



def test_python_class_definition():
    with Sandbox.create() as sbx:
        sbx.code.run(
            "class Counter:\n"
            "    def __init__(self):\n"
            "        self.count = 0\n"
            "    def increment(self):\n"
            "        self.count += 1\n"
            "        return self.count"
        )
        sbx.code.run("c = Counter()")
        sbx.code.run("c.increment()")
        sbx.code.run("c.increment()")
        result = sbx.code.run("print(c.count)")
        assert result.success
        assert "2" in result.text

def test_mount_read_only(tmp_path):
    test_file = tmp_path / "hello.txt"
    test_file.write_text("mount_content\n")

    with Sandbox.create(mounts={str(tmp_path): "/mnt/data"}) as sbx:
        result = sbx.commands.run("cat /mnt/data/hello.txt")
        assert result.success
        assert "mount_content" in result.stdout


def test_mount_read_write(tmp_path):
    with Sandbox.create(mounts={str(tmp_path): "/mnt/out:rw"}) as sbx:
        sbx.commands.run("echo written_from_guest > /mnt/out/output.txt")

    assert (tmp_path / "output.txt").read_text().strip() == "written_from_guest"

def test_snapshot_list():
   snapshotlist = snapshots.catalog()

   print(snapshotlist)
   assert isinstance(snapshotlist, list)
   assert len(snapshotlist) > 0


def test_interactive_python_does_not_kill_the_session():
    with Sandbox.create() as sbx:
        sbx.commands.run("export MARKER=kept")

        started = time.time()
        interrupted = sbx.commands.run("python3", timeout=10)
        elapsed = time.time() - started

        assert interrupted.exit_code == 0, "python3 should read EOF and exit"
        assert elapsed < 5, f"python3 waited {elapsed:.2f}s, so stdin was not closed"

        after = sbx.commands.run("echo $MARKER")
        assert after.success
        assert "kept" in after.stdout


def test_a_command_that_outlives_its_timeout_does_not_kill_the_session():
    """The recovery path itself, with a command that still cannot finish."""
    with Sandbox.create() as sbx:
        sbx.commands.run("export MARKER=kept")

        assert sbx.commands.run("sleep 300", timeout=3).exit_code == 124

        after = sbx.commands.run("echo $MARKER")
        assert after.success
        assert "kept" in after.stdout


def test_commands_run_is_not_a_python_repl():
    with Sandbox.create() as sbx:
        sbx.commands.run("python3", timeout=3)

        assert sbx.commands.run("print(x)").exit_code != 0

        sbx.code.run("x = 1")
        assert sbx.code.run("print(x)").text.strip() == "1"


def test_heredoc_writes_a_file():
    with Sandbox.create() as sbx:
        written = sbx.commands.run("cat <<'EOF' > /tmp/written.py\nprint('written')\nEOF")
        assert written.success

        assert "written" in sbx.commands.run("python3 /tmp/written.py").stdout


def test_stderr_stays_on_its_own_stream():
    with Sandbox.create() as sbx:
        result = sbx.commands.run("echo out; echo err >&2")
        assert result.stdout.strip() == "out"
        assert result.stderr.strip() == "err"


def test_commands_ending_in_a_comment():
    with Sandbox.create() as sbx:
        cases = [
            ("echo #", 0, ""),
            ("ls -d / #", 0, "/"),
            ("pwd #", 0, "/"),
            ("basename /a/b #", 0, "b"),
            ("echo kept # a note after it", 0, "kept"),
            ("echo a; echo b #", 0, "a\nb"),
            ("echo '#' #", 0, "#"),
            # A real non-zero exit, not the 124 a timeout would report.
            ("false #", 1, ""),
        ]

        for command, exit_code, stdout in cases:
            result = sbx.commands.run(command, timeout=10)
            assert result.exit_code == exit_code, f"{command!r} exited {result.exit_code}"
            assert result.stdout.strip() == stdout, f"{command!r} printed {result.stdout!r}"


def test_the_command_after_a_timeout_is_not_slow():
    with Sandbox.create() as sbx:
        assert sbx.commands.run("sleep 300", timeout=3).exit_code == 124

        started = time.time()
        after = sbx.commands.run("echo back")
        elapsed = time.time() - started

        assert after.stdout.strip() == "back"
        assert elapsed < 3, f"the command after a timeout took {elapsed:.2f}s"


def test_a_command_that_pauses_its_output_keeps_running():
    with Sandbox.create() as sbx:
        cases = [
            ("echo a; sleep 2; echo b", "a\nb"),
            ("echo a; sleep 6; echo b", "a\nb"),
            ("for i in 1 2 3; do echo $i; sleep 1; done", "1\n2\n3"),
            ("echo a; awk 'BEGIN{for(i=0;i<100000;i++)x+=i}'; echo b", "a\nb"),
            ("sleep 2; echo done", "done"),
        ]

        for command, stdout in cases:
            result = sbx.commands.run(command, timeout=60)
            assert result.exit_code == 0, f"{command!r} exited {result.exit_code}"
            assert result.stdout.strip() == stdout, f"{command!r} printed {result.stdout!r}"


# --- interrupt ---
FOREVER = "sleep 300"
INTERRUPT_TIMEOUT = 60


def _interrupt_after(sbx, seconds):
    """Ask the command to stop from another thread, the way a terminal would."""
    timer = threading.Timer(seconds, sbx.commands.interrupt)
    timer.daemon = True
    timer.start()
    return timer


def test_interrupt_stops_a_running_command_with_130():
    """130 rather than 124 is the point: the command ends through the ordinary completion path, so the shell is left at a prompt with nothing to recover.
    """
    with Sandbox.create() as sbx:
        _interrupt_after(sbx, 0.5)

        started = time.time()
        result = sbx.commands.run(FOREVER, timeout=INTERRUPT_TIMEOUT)
        elapsed = time.time() - started

        assert result.exit_code == 130
        assert elapsed < 30, f"took {elapsed:.2f}s, so it was not interrupted"


def test_interrupt_keeps_the_output_produced_before_it():
    with Sandbox.create() as sbx:
        _interrupt_after(sbx, 0.5)

        result = sbx.commands.run(f"echo working; {FOREVER}", timeout=INTERRUPT_TIMEOUT)

        assert result.exit_code == 130
        assert result.stdout.strip() == "working"


def test_interrupt_leaves_the_session_usable():
    with Sandbox.create() as sbx:
        sbx.commands.run("export MARKER=kept")

        _interrupt_after(sbx, 0.5)
        sbx.commands.run(FOREVER, timeout=INTERRUPT_TIMEOUT)

        after = sbx.commands.run("echo $MARKER")
        assert after.exit_code == 0
        assert after.stdout.strip() == "kept"


def test_interrupt_with_nothing_running_is_a_no_op():
    """A stray Ctrl-C at an idle prompt must not reach the guest: the byte would sit in the receive queue and be processed at the head of the next command, whose output would then carry a ^C echo and a stray prompt.
    """
    with Sandbox.create() as sbx:
        sbx.commands.run("true")
        sbx.commands.interrupt()
        sbx.commands.interrupt()

        after = sbx.commands.run("echo clean")
        assert after.exit_code == 0
        assert after.stdout.strip() == "clean"


def test_an_abandoned_sliced_command_does_not_brick_the_session():
    from vpod.commands import SLICE_NANOS
    from vpod._result import unwrap_result

    with Sandbox.create() as sbx:
        sbx.commands.run("true")

        session_id = sbx._get_shell_session_id()
        started = unwrap_result(
            sbx._exports["session-exec-slice"](session_id, "sleep 30", 30, SLICE_NANOS, "closed")
        )
        assert (
            getattr(started, "exit-code") is None
        ), "sleep 30 cannot have finished in one slice"

        recovered = sbx.commands.run("echo recovered", timeout=10)
        assert recovered.exit_code == 0
        assert recovered.stdout.strip() == "recovered"

        again = sbx.commands.run("echo second", timeout=10)
        assert again.stdout.strip() == "second"


def test_a_failed_call_reports_what_went_wrong():
    with Sandbox.create() as sbx:
        sbx.commands.run("true")

        with pytest.raises(RuntimeError, match="invalid session handle"):
            from vpod._result import unwrap_result
            unwrap_result(sbx._exports["session-exec"](9999, "echo x", 10))


def test_stdin_is_closed_so_a_reader_gets_eof_instead_of_hanging():
    with Sandbox.create() as sbx:
        sbx.commands.run("true")

        cases = [
            ("echo a; read x", 1, "a"),
            ("echo a; cat", 0, "a"),
            ("read x", 1, ""),
            ("python3", 0, ""),
        ]
        for command, exit_code, stdout in cases:
            started = time.time()
            result = sbx.commands.run(command, timeout=10)
            elapsed = time.time() - started

            assert result.exit_code == exit_code, f"{command!r} exited {result.exit_code}"
            assert result.stdout.strip() == stdout, f"{command!r} printed {result.stdout!r}"
            assert elapsed < 5, f"{command!r} waited {elapsed:.2f}s, so stdin was not closed"


def test_a_command_that_cannot_finish_can_be_interrupted():
    with Sandbox.create() as sbx:
        sbx.commands.run("true")

        for command in (FOREVER, "awk 'BEGIN{for(i=0;i<100000000;i++)x+=i}'"):
            _interrupt_after(sbx, 0.5)

            started = time.time()
            result = sbx.commands.run(command, timeout=INTERRUPT_TIMEOUT)
            elapsed = time.time() - started

            assert result.exit_code == 130, f"{command!r} exited {result.exit_code}"
            assert elapsed < 30, f"{command!r} took {elapsed:.2f}s, so it was not interrupted"

        assert sbx.commands.run("echo alive").stdout.strip() == "alive"


def test_two_sandboxes_do_not_share_one_random_stream():
    probe = (
        "import os, secrets, uuid\n"
        "print(os.urandom(16).hex(), secrets.token_hex(16), uuid.uuid4())"
    )

    samples = []
    for _ in range(2):
        with Sandbox.create() as sbx:
            result = sbx.code.run(probe, timeout=120)
            assert result.success, f"the probe failed: {result.error}"
            samples.append(result.text.strip())

    first, second = samples
    assert first and second, "the probe printed nothing"
    assert first != second, (
        f"both sandboxes produced {first!r}. The guest crng was restored from the "
        f"snapshot and never re-keyed, so every sandbox from this image shares one "
        f"stream of uuids, tokens and keys."
    )


def test_the_shell_gets_the_same_reseed_as_the_interpreter():
    samples = []
    for _ in range(2):
        with Sandbox.create() as sbx:
            result = sbx.commands.run("head -c 16 /dev/urandom | od -An -tx1", timeout=60)
            assert result.exit_code == 0, result.stderr
            samples.append(result.stdout.strip())

    first, second = samples
    assert first and second, "the read returned nothing"
    assert first != second, f"both shells read {first!r} from /dev/urandom"

def test_two_sandboxes_do_not_share_one_mersenne_twister():
    samples = []
    for _ in range(2):
        with Sandbox.create() as sbx:
            result = sbx.code.run("import random\nprint(random.random())", timeout=120)
            assert result.success, f"the probe failed: {result.error}"
            samples.append(result.text.strip())

    first, second = samples
    assert first and second, "the probe printed nothing"
    assert first != second, (
        f"both sandboxes produced {first!r}. The interpreter was restored with random "
        f"already imported, so its Mersenne Twister state came from the snapshot."
    )


def test_two_sandboxes_do_not_share_one_numpy_global():
    probe = (
        "try:\n"
        "    import numpy\n"
        "except ImportError:\n"
        "    print('NO_NUMPY')\n"
        "else:\n"
        "    print(numpy.random.rand(), numpy.random.randint(0, 10**9))"
    )

    samples = []
    for _ in range(2):
        with Sandbox.create() as sbx:
            result = sbx.code.run(probe, timeout=180)
            assert result.success, f"the probe failed: {result.error}"
            samples.append(result.text.strip())

    first, second = samples
    if first == "NO_NUMPY":
        pytest.skip("this snapshot does not carry numpy")

    assert first != second, (
        f"both sandboxes produced {first!r}. numpy.random's legacy global was seeded "
        f"when the snapshot warm-imported it, so reseeding the kernel pool and random "
        f"alone leaves it shared."
    )


def test_two_shells_do_not_share_one_random_variable():
    samples = []
    for _ in range(2):
        with Sandbox.create() as sbx:
            result = sbx.commands.run("echo $RANDOM $RANDOM $RANDOM", timeout=60)
            assert result.exit_code == 0, result.stderr
            samples.append(result.stdout.strip())

    first, second = samples
    assert first and second, "the shell printed nothing"
    assert first != second, (
        f"both shells produced {first!r}. RANDOM is the shell's own generator, seeded "
        f"before the snapshot was taken, so it survives a kernel pool reseed."
    )


def test_a_seed_the_caller_sets_on_purpose_survives():
    with Sandbox.create() as sbx:
        seeded = sbx.code.run("import random\nrandom.seed(42)", timeout=120)
        assert seeded.success, f"seeding failed: {seeded.error}"

        first = sbx.code.run("print(random.random())", timeout=120)
        assert first.success, f"the probe failed: {first.error}"

        reference = sbx.code.run(
            "import random\nrandom.seed(42)\nprint(random.random())", timeout=120
        )

        assert first.text.strip() == reference.text.strip(), (
            "a reseed ran between two calls in one sandbox and threw away the caller's "
            "own random.seed(42)"
        )


def test_an_unfinishable_command_waits_for_its_timeout_rather_than_guessing():
    with Sandbox.create() as sbx:
        started = time.time()
        result = sbx.commands.run("echo a; sleep 300", timeout=3)
        elapsed = time.time() - started

        assert result.exit_code == 124
        assert result.stdout.strip() == "a", "output before the wait is still reported"
        assert elapsed >= 2.5, f"returned after {elapsed:.2f}s, so something guessed again"
        assert elapsed < 15, f"took {elapsed:.2f}s, far past the timeout asked for"

        assert sbx.commands.run("echo alive").stdout.strip() == "alive"


# --- streaming ---

@pytest.mark.integration
def test_output_arrives_in_more_than_one_chunk():
    with Sandbox.create() as sbx:
        chunks = []
        result = sbx.commands.run(
            "for i in 1 2 3 4; do echo line$i; sleep 1; done",
            timeout=60,
            on_stdout=chunks.append,
        )

        assert result.exit_code == 0
        assert result.stdout.strip() == "line1\nline2\nline3\nline4"

        assert len(chunks) > 1, f"got one chunk: {chunks!r}"


@pytest.mark.integration
def test_the_chunks_concatenate_to_what_the_callback_free_call_returns():
    command = "for i in 1 2 3; do echo $i; sleep 1; done"

    with Sandbox.create() as sbx:
        chunks = []
        streamed = sbx.commands.run(command, timeout=60, on_stdout=chunks.append)
        plain = sbx.commands.run(command, timeout=60)

        assert "".join(chunks).rstrip() == streamed.stdout
        assert streamed.stdout == plain.stdout


@pytest.mark.integration
def test_stderr_streams_on_its_own_callback():
    with Sandbox.create() as sbx:
        out, err = [], []
        result = sbx.commands.run(
            "echo to-stdout; echo to-stderr >&2",
            timeout=30,
            on_stdout=out.append,
            on_stderr=err.append,
        )

        assert result.exit_code == 0
        assert "to-stdout" in "".join(out)
        assert "to-stderr" in "".join(err)
        assert "to-stderr" not in "".join(out)


@pytest.mark.integration
def test_an_interrupt_keeps_the_chunks_that_already_arrived():
    with Sandbox.create() as sbx:
        chunks = []

        def stop_after_two(chunk):
            chunks.append(chunk)
            if len(chunks) == 2:
                sbx.commands.interrupt()

        result = sbx.commands.run(
            "for i in $(seq 1 60); do echo tick$i; sleep 1; done",
            timeout=120,
            on_stdout=stop_after_two,
        )

        assert result.exit_code == 130, f"exited {result.exit_code}"
        assert "tick1" in result.stdout
        assert len(chunks) >= 2


# --- stdin and terminal mode ---------------------------------------------

def test_a_command_can_be_written_to():
    with Sandbox.create() as sbx:
        result = sbx.commands.run("cat", stdin="hello stdin\n", timeout=30)
        assert result.exit_code == 0
        assert "hello stdin" in result.stdout


def test_stdin_is_still_closed_by_default():
    with Sandbox.create() as sbx:
        result = sbx.commands.run("cat", timeout=20)
        assert result.exit_code == 0
        assert result.stdout == ""


def test_a_repl_prompt_arrives_and_answers_over_a_stream():
    with Sandbox.create() as sbx:
        inbox = queue.Queue()
        inbox.put("print(40 + 2)\n")
        inbox.put("exit()\n")

        result = sbx.commands.run("python3 2>&1", stdin=inbox, timeout=60)

        assert ">>>" in result.stdout, f"never saw a prompt, got {result.stdout!r}"
        assert "42" in result.stdout, f"REPL did not answer, got {result.stdout!r}"
        assert result.exit_code == 0


def test_closing_a_stream_before_the_command_reads_does_not_end_it():
    with Sandbox.create() as sbx:
        inbox = queue.Queue()
        inbox.put("print(40 + 2)\n")
        inbox.put(None)

        result = sbx.commands.run("python3 2>&1", stdin=inbox, timeout=20)

        assert "42" in result.stdout, result.stdout
        assert result.exit_code == 124, "an early Ctrl-D is expected to be swallowed"


def test_a_stream_can_react_to_what_the_command_prints():
    with Sandbox.create() as sbx:
        inbox = queue.Queue()
        seen = []
        asked = []

        def on_stdout(chunk):
            seen.append(chunk)
            text = "".join(seen)
            if not asked and ">>>" in text:
                asked.append(True)
                inbox.put("print(6 * 7)\n")
            elif len(asked) == 1 and "42" in text:
                asked.append(True)
                inbox.put(None)

        result = sbx.commands.run(
            "python3 2>&1", stdin=inbox, on_stdout=on_stdout, timeout=60
        )

        assert len(asked) == 2, f"the feeder never caught up, got {result.stdout!r}"
        assert result.exit_code == 0


def test_an_interactive_python_can_be_interrupted():
    with Sandbox.create() as sbx:
        inbox = queue.Queue()
        inbox.put("import time\ntime.sleep(300)\n")

        threading.Timer(8.0, sbx.commands.interrupt).start()
        threading.Timer(14.0, lambda: inbox.put(None)).start()

        result = sbx.commands.run("python3 2>&1", stdin=inbox, timeout=90)

        assert "KeyboardInterrupt" in result.stdout, result.stdout
        assert result.stdout.rstrip().endswith(">>>"), "the REPL did not come back"
        assert result.exit_code in (0, 130), result.exit_code


def test_a_nested_shell_leaves_the_session_usable():
    with Sandbox.create() as sbx:
        inbox = queue.Queue()
        threading.Timer(2.0, inbox.put, args=("exit\n",)).start()

        nested = sbx.commands.run("sh", stdin=inbox, tty=True, timeout=30)
        assert nested.exit_code == 0, nested.stdout

        after = sbx.commands.run("expr 2 + 2", timeout=20)
        assert after.stdout == "4", after
        assert after.exit_code == 0
        assert "__ec" not in after.stdout


def test_the_prompt_is_not_exported_to_child_processes():
    with Sandbox.create() as sbx:
        child = sbx.commands.run("sh -c 'echo \"[$PS1]\"'", timeout=20)
        assert "__ec" not in child.stdout, child.stdout


def test_a_string_ends_the_input_even_on_a_terminal():
    with Sandbox.create() as sbx:
        result = sbx.commands.run("cat", stdin="hello\n", tty=True, timeout=30)
        assert result.exit_code == 0, "a string is finite, so it has to end with EOF"
        assert "hello" in result.stdout


def test_a_streamed_chunk_keeps_the_newline_the_program_wrote():
    with Sandbox.create() as sbx:
        chunks = []
        result = sbx.commands.run("/bin/echo hi", on_stdout=chunks.append, timeout=20)

        assert chunks == ["hi\n"], "the last chunk used to arrive trimmed"
        assert result.stdout == "hi"


def test_input_the_command_never_read_is_not_run_as_a_command():
    with Sandbox.create() as sbx:
        sbx.commands.run("rm -f /tmp/leftover-ran", timeout=20)

        result = sbx.commands.run(
            "head -1", stdin="kept\ntouch /tmp/leftover-ran\n", tty=True, timeout=30
        )
        assert "kept" in result.stdout

        verdict = sbx.commands.run(
            "if [ -e /tmp/leftover-ran ]; then echo EXECUTED; else echo clean; fi",
            timeout=20,
        )
        assert verdict.stdout.strip() == "clean", "leftover input reached the shell"


def test_the_prompt_sentinel_never_reaches_the_caller():
    with Sandbox.create() as sbx:
        result = sbx.commands.run(
            "head -2", stdin="alpha\nbeta\ngamma\n", tty=True, timeout=30
        )
        assert "\x1f" not in result.stdout, repr(result.stdout)

        after = sbx.commands.run("echo alive", timeout=20)
        assert after.stdout == "alive", repr(after.stdout)


def test_a_terminal_command_still_reports_its_own_exit_code():
    with Sandbox.create() as sbx:
        assert sbx.commands.run("sh -c 'exit 7'", tty=True, timeout=20).exit_code == 7
        assert sbx.commands.run("true", tty=True, timeout=20).exit_code == 0


def test_terminal_mode_restores_the_shell_for_the_next_command():
    with Sandbox.create() as sbx:
        sbx.commands.run("python3 -c 'input()'", stdin="x\n", tty=True, timeout=30)

        after = sbx.commands.run("echo still-here", timeout=20)
        assert after.exit_code == 0
        assert after.stdout.strip() == "still-here"


def test_a_python_script_can_read_the_terminal():
    with Sandbox.create() as sbx:
        has_fix = sbx.commands.run(
            "grep -c caller_process_group /usr/lib/vpod/pydaemon.py || true", timeout=20
        )
        if has_fix.stdout.strip() in ("", "0"):
            pytest.skip("snapshot predates the pydaemon process-group fix")

        result = sbx.commands.run(
            "python3 -c 'import sys; print(\"GOT\", sys.stdin.readline().strip())'",
            stdin="hello\n",
            tty=True,
            timeout=30,
        )
        assert "GOT hello" in result.stdout, result.stdout
        assert "I/O error" not in result.stdout


def test_a_command_that_stops_early_does_not_kill_the_session():
    reads_to_eof = ["cat", "sort", "wc -l"]
    stops_early = ["head -1", "grep -m1 beta", "sh -c 'read x; echo $x'",
                   "python3 -c 'print(input())'"]

    with Sandbox.create() as sbx:
        for command in reads_to_eof + stops_early:
            result = sbx.commands.run(command, stdin="alpha\nbeta\ngamma\n", timeout=30)
            assert result.exit_code == 0, f"{command}: {result}"

            alive = sbx.commands.run("echo alive", timeout=15)
            assert alive.exit_code == 0, f"{command} killed the session"
            assert alive.stdout.strip() == "alive", f"{command} left {alive.stdout!r}"


def test_staged_stdin_does_not_leak_into_the_next_command():
    with Sandbox.create() as sbx:
        first = sbx.commands.run("head -1", stdin="one\ntwo\nthree\n", timeout=25)
        assert first.stdout.strip() == "one"

        second = sbx.commands.run("echo done", timeout=15)
        assert second.stdout.strip() == "done", second.stdout
        assert "two" not in second.stdout and "three" not in second.stdout


def test_stdin_survives_being_larger_than_one_staging_chunk():
    payload = ("x" * 63 + "\n") * 400
    with Sandbox.create() as sbx:
        result = sbx.commands.run("wc -c", stdin=payload, timeout=180)
        assert result.exit_code == 0, result
        assert result.stdout.strip().split()[0] == str(len(payload)), result.stdout


def test_stdin_is_delivered_byte_for_byte():
    import base64 as _b64

    raw = bytes(range(256)) * 4
    with Sandbox.create() as sbx:
        result = sbx.commands.run("base64", stdin=raw, timeout=120)
        assert result.exit_code == 0, result
        assert _b64.b64decode(result.stdout.replace("\n", "")) == raw
