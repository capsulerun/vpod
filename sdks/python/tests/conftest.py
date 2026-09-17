import json

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
    if request.node.get_closest_marker("integration") or request.node.get_closest_marker(
        "performance"
    ):
        return

    store = MagicMock()
    sessions = {}
    session_counter = {"id": 0}
    stdin_writes = []
    traced_sessions = {}

    def fake_execute(snapshot_path, command):
        import subprocess
        result = subprocess.run(command, shell=True, capture_output=True, text=True)
        return FakeRecord(
            stdout=result.stdout.strip(),
            stderr=result.stderr,
            **{"exit-code": result.returncode},
        )

    def fake_session_start(snapshot_path, command, prompt, mounts=None):
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

    def record_exec(sid, command):
        session = traced_sessions.get(sid)
        if session is None or command is None:
            return
        event = {
            "v": 1,
            "seq": session["seq"],
            "guest_ns": session["seq"],
            "wall_ms": 0,
            "kind": "process.exec",
            "task": f"{sid:x}",
            "path": "/bin/sh",
            "argv": ["sh", "-c", command],
        }
        session["seq"] += 1
        session["pending"].append((json.dumps(event) + "\n").encode())

    def fake_session_trace_start(sid, options):
        traced_sessions[sid] = {"options": options, "pending": [], "seq": 0}
        return FakeVariant(tag="ok", payload=None)

    def fake_session_trace_drain(sid, max_bytes):
        session = traced_sessions.get(sid)
        if session is None:
            return FakeVariant(tag="err", payload="tracing is not enabled for this session")
        pending, session["pending"] = session["pending"], []
        return FakeVariant(tag="ok", payload=b"".join(pending))

    def fake_session_trace_stop(sid):
        traced_sessions.pop(sid, None)
        return FakeVariant(tag="ok", payload=None)

    def fake_session_exec_slice(sid, command, timeout=None, slice_nanos=0, mode="closed"):
        record_exec(sid, command)
        payload = fake_session_exec(sid, command).payload
        return FakeVariant(
            tag="ok",
            payload=FakeRecord(stdout=payload, stderr="", **{"exit-code": 0}),
        )

    def fake_session_interrupt(sid):
        return FakeVariant(tag="ok", payload=None)

    def fake_session_stdin(sid, data):
        stdin_writes.append((sid, bytes(data)))
        return FakeVariant(tag="ok", payload=None)

    exports = {
        "execute": fake_execute,
        "session-start": fake_session_start,
        "session-exec": fake_session_exec,
        "session-exec-slice": fake_session_exec_slice,
        "session-interrupt": fake_session_interrupt,
        "session-stdin": fake_session_stdin,
        "session-close": fake_session_close,
        "session-trace-start": fake_session_trace_start,
        "session-trace-drain": fake_session_trace_drain,
        "session-trace-stop": fake_session_trace_stop,
    }

    from vpod.snapshots import PulledSnapshot

    monkeypatch.setattr(
        "vpod.snapshots.pull",
        lambda name="alpine:latest", **kwargs: Path("/fake/snapshot.snap"),
    )
    monkeypatch.setattr(
        "vpod.snapshots._pull",
        lambda name="alpine:latest", *args, **kwargs: PulledSnapshot(
            Path("/fake/snapshot.snap"), None, None, None
        ),
    )
    monkeypatch.setattr("vpod.sandbox.locate_wasm", lambda: Path("/fake/vpod_wasi_lib.wasm"))
    monkeypatch.setattr(
        "vpod.sandbox.load_component",
        lambda path, snap=None, mounts=None, **kwargs: (store, exports),
    )

    return {"exports": exports, "stdin_writes": stdin_writes, "traced_sessions": traced_sessions}
