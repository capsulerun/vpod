import pytest
from pathlib import Path
from unittest.mock import MagicMock


class FakeRecord:
    def __init__(self, **kwargs):
        for k, v in kwargs.items():
            setattr(self, k, v)


class FakeVariant:
    def __init__(self, tag, payload):
        self.tag = tag
        self.payload = payload


@pytest.fixture(autouse=True)
def mock_component(request, monkeypatch):
    if request.node.get_closest_marker("integration"):
        return

    store = MagicMock()
    sessions = {}
    session_counter = {"id": 0}

    def fake_execute(snapshot_path, command):
        import subprocess
        result = subprocess.run(command, shell=True, capture_output=True, text=True)
        return FakeRecord(
            stdout=result.stdout.strip(),
            stderr=result.stderr,
            **{"exit-code": result.returncode},
        )

    def fake_session_start(snapshot_path, command, prompt):
        session_counter["id"] += 1
        sid = session_counter["id"]
        sessions[sid] = {"env": {}, "type": command}
        return sid

    def fake_session_exec(sid, command):
        import subprocess
        session = sessions.get(sid, {})

        if session.get("type") == "python3":
            result = subprocess.run(
                ["python3", "-c", command],
                capture_output=True, text=True,
            )
            output = result.stdout.strip()
            if result.stderr:
                output = (output + "\n" + result.stderr).strip()
            return FakeVariant(tag="ok", payload=output)

        import os
        import re
        env = session.get("env", {})
        full_env = {**os.environ, **env}
        result = subprocess.run(command, shell=True, capture_output=True, text=True, env=full_env)

        for match in re.finditer(r"export\s+(\w+)=(\S+)", command):
            env[match.group(1)] = match.group(2)
        session["env"] = env

        output = result.stdout.strip() + result.stderr
        return FakeVariant(tag="ok", payload=output)

    def fake_session_close(sid):
        sessions.pop(sid, None)

    exports = {
        "execute": fake_execute,
        "session-start": fake_session_start,
        "session-exec": fake_session_exec,
        "session-close": fake_session_close,
    }

    monkeypatch.setattr("vpod.snapshots.pull", lambda name="alpine:latest": Path("/fake/snapshot.snap"))
    monkeypatch.setattr("vpod.sandbox.locate_wasm", lambda: Path("/fake/vpod_wasi_lib.wasm"))
    monkeypatch.setattr("vpod.sandbox.load_component", lambda path, snap=None: (store, exports))
