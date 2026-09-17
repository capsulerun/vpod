import pytest

from vpod import Sandbox

pytestmark = pytest.mark.integration


def test_a_shell_script_records_the_commands_it_runs_and_the_files_it_writes():
    script = "mkdir -p /tmp/traced && echo hi > /tmp/traced/out.txt && cat /tmp/traced/out.txt"
    with Sandbox.create(trace=True) as sbx:
        result = sbx.commands.run(f"sh -c '{script}'")
        assert result.success

        commands = [node.argv for node in result.trace.processes()]
        assert commands[0] == ["sh", "-c", script]
        assert any(argv[0] == "cat" for argv in commands), commands

        written = [file.path for file in result.trace.files() if file.written]
        assert "/tmp/traced/out.txt" in written, written
        assert result.trace.complete


def test_code_run_activity_belongs_to_the_user_not_to_vpod():
    with Sandbox.create(trace=True) as sbx:
        execution = sbx.code.run("open('/tmp/from_code.txt', 'w').write('x')")
        assert execution.success, execution.error

        written = [file.path for file in execution.trace.files() if file.written]
        assert "/tmp/from_code.txt" in written, written


def test_each_command_keeps_its_own_events_and_the_sandbox_keeps_them_all():
    with Sandbox.create(trace=True) as sbx:
        first = sbx.commands.run("touch /tmp/first")
        second = sbx.commands.run("touch /tmp/second")

        def paths(trace):
            return [file.path for file in trace.files()]

        assert "/tmp/first" in paths(first.trace)
        assert "/tmp/second" not in paths(first.trace)
        assert "/tmp/second" in paths(second.trace)
        assert {"/tmp/first", "/tmp/second"} <= set(paths(sbx.trace.collect()))


def test_vpod_plumbing_is_hidden_unless_asked_for():
    with Sandbox.create(trace=True) as sbx:
        trace = sbx.commands.run("true").trace

        assert "/dev/ttyS1" not in [file.path for file in trace.files(noise=True)]
        assert "/dev/ttyS1" in [file.path for file in trace.files(internal=True, noise=True)]


def test_a_resumed_sandbox_starts_a_fresh_trace():
    with Sandbox.create(trace=True) as sbx:
        sbx.commands.run("touch /tmp/before-suspend")
        instance_id = sbx.suspend()
        assert "/tmp/before-suspend" in [file.path for file in sbx.trace.collect().files()]

    resumed = Sandbox.resume(instance_id, trace=True)
    try:
        after = resumed.commands.run("touch /tmp/after-resume")
        assert "/tmp/after-resume" in [file.path for file in after.trace.files()]
        assert "/tmp/before-suspend" not in [file.path for file in resumed.trace.collect().files()]
    finally:
        resumed.close()
