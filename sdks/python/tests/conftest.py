import pytest
from pathlib import Path
from unittest.mock import MagicMock


@pytest.fixture(autouse=True)
def mock_component(monkeypatch):
    """Mock the WASM component so tests run without a real .wasm file or registry."""

    store = MagicMock()
    sessions = {}
    session_counter = {"id": 0}

    def fake_execute(_store, snapshot_path, command):
        import subprocess
        result = subprocess.run(command, shell=True, capture_output=True, text=True)
        return {"is_err": False, "ok": {
            "stdout": result.stdout,
            "stderr": result.stderr,
            "exit_code": result.returncode,
        }}

    def fake_session_start(_store, snapshot_path, command, prompt):
        session_counter["id"] += 1
        sid = session_counter["id"]
        sessions[sid] = {"env": {}}
        return {"is_err": False, "ok": sid}

    def fake_session_exec(_store, sid, command):
        import subprocess
        session = sessions.get(sid, {})
        env = session.get("env", {})

        is_python = any(token in command for token in ["print(", "import ", "def ", "/", "(", ")"])
        if is_python and not command.startswith(("ls", "echo", "export", "cd", "touch", "cat")):
            result = subprocess.run(
                ["python3", "-c", command],
                capture_output=True, text=True, env={**__import__("os").environ, **env}
            )
            return {"is_err": False, "ok": result.stdout + result.stderr}

        import os
        import re
        full_env = {**os.environ, **env}
        result = subprocess.run(command, shell=True, capture_output=True, text=True, env=full_env)

        for match in re.finditer(r"export\s+(\w+)=(\S+)", command):
            env[match.group(1)] = match.group(2)
        session["env"] = env

        return {"is_err": False, "ok": result.stdout + result.stderr}

    def fake_session_close(_store, sid):
        sessions.pop(sid, None)

    exports = {
        "execute": fake_execute,
        "session-start": fake_session_start,
        "session-exec": fake_session_exec,
        "session-close": fake_session_close,
    }

    monkeypatch.setattr("vpod.snapshots.pull", lambda name="alpine:latest": Path("/fake/snapshot.snap"))
    monkeypatch.setattr("vpod.sandbox.locate_wasm", lambda: Path("/fake/vpod_wasi_lib.wasm"))
    monkeypatch.setattr("vpod.sandbox.load_component", lambda path: (store, exports))
