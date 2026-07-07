import pytest
from vpod import Sandbox

pytestmark = pytest.mark.integration


def test_suspend_returns_instance_id():
    with Sandbox.create() as sbx:
        sbx.commands.run("echo warmup")
        instance_id = sbx.suspend()
        assert instance_id is not None
        assert len(instance_id) == 36


def test_suspend_and_resume_preserves_env():
    with Sandbox.create() as sbx:
        sbx.commands.run("export MY_VAR=persisted")
        instance_id = sbx.suspend()

    sbx = Sandbox.resume(instance_id)
    result = sbx.commands.run("echo $MY_VAR")
    assert "persisted" in result.stdout
    sbx.close()


def test_suspend_and_resume_preserves_files():
    with Sandbox.create() as sbx:
        sbx.commands.run("echo hello > /tmp/survive.txt")
        instance_id = sbx.suspend()

    sbx = Sandbox.resume(instance_id)
    result = sbx.commands.run("cat /tmp/survive.txt")
    assert "hello" in result.stdout
    sbx.close()


def test_list_sessions_shows_suspended():
    with Sandbox.create() as sbx:
        sbx.commands.run("echo test")
        instance_id = sbx.suspend()

    instances = Sandbox.list_sessions()
    ids = [e["id"] for e in instances]
    assert instance_id in ids

    entry = next(e for e in instances if e["id"] == instance_id)
    assert entry["state"] == "SUSPENDED"


def test_resume_updates_state_to_running():
    with Sandbox.create() as sbx:
        instance_id = sbx.suspend()

    sbx = Sandbox.resume(instance_id)

    instances = Sandbox.list_sessions()
    entry = next(e for e in instances if e["id"] == instance_id)
    assert entry["state"] == "RUNNING"
    sbx.close()


def test_destroy_removes_instance():
    with Sandbox.create() as sbx:
        instance_id = sbx.suspend()

    Sandbox.destroy(instance_id)

    instances = Sandbox.list_sessions()
    ids = [e["id"] for e in instances]
    assert instance_id not in ids


def test_resume_nonexistent_instance_fails():
    with pytest.raises(Exception):
        Sandbox.resume("nonexistent-id-000-000-000000000000")


def test_multiple_suspend_resume_cycles():
    with Sandbox.create() as sbx:
        sbx.commands.run("export COUNTER=1")
        id1 = sbx.suspend()

    sbx = Sandbox.resume(id1)
    sbx.commands.run("export COUNTER=2")
    id2 = sbx.suspend()

    sbx = Sandbox.resume(id2)
    result = sbx.commands.run("echo $COUNTER")
    assert "2" in result.stdout
    sbx.close()


def test_suspend_resume_python_state():
    with Sandbox.create() as sbx:
        sbx.code.run("x = 42")
        sbx.code.run("data = [1, 2, 3]")
        instance_id = sbx.suspend()

    sbx = Sandbox.resume(instance_id)
    result = sbx.code.run("print(x + sum(data))")
    assert "48" in result.text
    sbx.close()


def test_independent_instances_isolated():
    with Sandbox.create() as sbx1:
        sbx1.commands.run("echo instance1 > /tmp/who.txt")
        id1 = sbx1.suspend()

    with Sandbox.create() as sbx2:
        sbx2.commands.run("echo instance2 > /tmp/who.txt")
        id2 = sbx2.suspend()

    restored1 = Sandbox.resume(id1)
    restored2 = Sandbox.resume(id2)

    r1 = restored1.commands.run("cat /tmp/who.txt")
    r2 = restored2.commands.run("cat /tmp/who.txt")

    assert "instance1" in r1.stdout
    assert "instance2" in r2.stdout

    restored1.close()
    restored2.close()
