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
