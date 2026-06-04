import pytest
from vpod import Sandbox

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


def test_code_requires_session():
    sbx = Sandbox.create()
    with pytest.raises(RuntimeError, match="requires a session"):
        sbx.code.run("print(1)")
