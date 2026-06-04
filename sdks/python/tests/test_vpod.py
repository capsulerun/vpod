import pytest
from vpod import Sandbox


@pytest.fixture
def sandbox():
    return Sandbox.create()


def test_stateless_command_returns_stdout(sandbox):
    result = sandbox.commands.run("echo hello")
    assert result.success
    assert "hello" in result.stdout


def test_stateless_command_captures_exit_code(sandbox):
    result = sandbox.commands.run("false")
    assert not result.success
    assert result.exit_code == 1


def test_stateless_command_no_shared_state():
    sbx = Sandbox.create()
    sbx.commands.run("export FOO=bar")
    result = sbx.commands.run("echo $FOO")
    assert result.stdout.strip() == ""


def test_session_commands_share_state():
    with Sandbox.create() as sbx:
        sbx.commands.run("export FOO=bar")
        result = sbx.commands.run("echo $FOO")
        assert "bar" in result.stdout


def test_session_filesystem_persists():
    with Sandbox.create() as sbx:
        sbx.commands.run("touch /tmp/hello.txt")
        result = sbx.commands.run("ls /tmp")
        assert "hello.txt" in result.stdout


def test_session_code_run():
    with Sandbox.create() as sbx:
        result = sbx.code.run("print(2 + 2)")
        assert result.success
        assert "4" in result.text


def test_session_code_captures_error():
    with Sandbox.create() as sbx:
        result = sbx.code.run("1 / 0")
        assert not result.success
        assert result.error is not None


def test_code_requires_session(sandbox):
    with pytest.raises(RuntimeError, match="requires a session"):
        sandbox.code.run("print(1)")
