import json
import os
import uuid
from os.path import abspath
from pathlib import Path
from typing import Optional

from . import snapshots
from .snapshots import cache_dir
from ._component import load_component, locate_wasm
from ._result import unwrap_result as _unwrap_result
from .code import Code
from .commands import Commands

INSTANCES_DIR = Path.home() / ".vpod" / "instances"


def _parse_mounts(mounts: dict[str, str]) -> list[dict]:
    result = []

    for host_path, guest_spec in mounts.items():
        writable = False
        guest_path = guest_spec

        if guest_spec.endswith(":rw"):
            writable = True
            guest_path = guest_spec[:-3]

        result.append({
            "host_path": abspath(host_path),
            "guest_path": guest_path,
            "writable": writable,
        })

    return result

_DEFAULT_SHELL = "/bin/sh"
_DEFAULT_PROMPT = "# "


class Sandbox:
    """
    Stateless usage:
        sandbox = Sandbox.create()
        result = sandbox.commands.run("echo hello")

    Persistent session:
        with Sandbox.create() as sandbox:
            sandbox.commands.run("export FOO=bar")
            result = sandbox.commands.run("echo $FOO")

            execution = sandbox.code.run("print(2 + 2)")
            print(execution.text)  # 4
    """

    def __init__(self, snapshot: str = "alpine:latest", mounts: dict[str, str] | None = None):
        snapshot_path = snapshots.pull(snapshot)
        wasm_path = locate_wasm()

        self._snapshot_path = "snap/" + snapshot_path.name
        self._mounts = _parse_mounts(mounts) if mounts else []

        mount_dirs = [m["host_path"] for m in self._mounts]
        self._store, self._exports = load_component(wasm_path, snapshot_path, mount_dirs or None)
        self._shell_session_id: Optional[int] = None
        self._in_context = False

        self.commands = Commands(
            self._exports,
            self._snapshot_path,
            self._get_shell_session_id,
        )

        self.code = Code(
            self._exports,
            self._snapshot_path,
            self._get_code_session_id,
        )

    @classmethod
    def create(cls, snapshot: str = "vsnap-base:latest", mounts: dict[str, str] | None = None) -> "Sandbox":
        return cls(snapshot, mounts=mounts)

    def _get_shell_session_id(self) -> int:
        if self._shell_session_id is None:
            mount_entries = []
            for i, m in enumerate(self._mounts):
                entry = object.__new__(type("MountEntry", (), {}))
                object.__setattr__(entry, "host-alias", f"mount{i}")
                object.__setattr__(entry, "guest-path", m["guest_path"])
                object.__setattr__(entry, "writable", m["writable"])
                mount_entries.append(entry)

            result = self._exports["session-start"](
                self._snapshot_path, _DEFAULT_SHELL, _DEFAULT_PROMPT, mount_entries
            )
            self._shell_session_id = int(_unwrap_result(result))
        return self._shell_session_id

    def _get_code_session_id(self) -> Optional[int]:
        if not self._in_context:
            return None
        return self._get_shell_session_id()

    def __enter__(self) -> "Sandbox":
        self._in_context = True
        return self

    def __exit__(self, *_) -> None:
        self.code.close()
        self._in_context = False
        if self._shell_session_id is not None:
            self._exports["session-close"](self._shell_session_id)
            self._shell_session_id = None

    def close(self) -> None:
        self.__exit__()

    def _snapshot_sha256(self) -> str:
        snapshot_name = self._snapshot_path.removeprefix("snap/")
        snap_file = cache_dir() / snapshot_name
        meta_file = snap_file.with_suffix(".meta")
        if meta_file.exists():
            return meta_file.read_text().strip()
        return ""

    def suspend(self) -> str:
        session_id = self._get_shell_session_id()

        instance_id = str(uuid.uuid4())
        instance_dir = INSTANCES_DIR / instance_id
        instance_dir.mkdir(parents=True, exist_ok=True)

        delta_rel = f"instances/{instance_id}/delta.bin"
        _unwrap_result(self._exports["session-suspend"](session_id, delta_rel))

        (instance_dir / "meta.json").write_text(json.dumps({
            "snapshot": self._snapshot_path,
            "snapshot_sha256": self._snapshot_sha256(),
            "mounts": self._mounts,
            "state": "SUSPENDED",
        }))

        self._shell_session_id = None
        self._update_manifest(instance_id, "SUSPENDED")
        return instance_id

    @classmethod
    def resume(cls, instance_id: str, mounts: dict[str, str] | None = None) -> "Sandbox":
        instance_dir = INSTANCES_DIR / instance_id
        meta = json.loads((instance_dir / "meta.json").read_text())
        delta_rel = f"instances/{instance_id}/delta.bin"


        snapshot_file = meta["snapshot"].removeprefix("snap/")
        override = os.environ.get("VPOD_SNAPSHOT")
        if override and Path(override).exists():
            snapshot_path = Path(override)
        else:
            cached = cache_dir() / snapshot_file
            snapshot_path = (
                cached if cached.exists()
                else snapshots.pull(snapshot_file.removesuffix(".snap"))
            )

        expected_hash = meta.get("snapshot_sha256", "")
        if expected_hash:
            meta_file = snapshot_path.with_suffix(".meta")
            current_hash = meta_file.read_text().strip() if meta_file.exists() else ""
            if current_hash and current_hash != expected_hash:
                raise RuntimeError(
                    f"Snapshot changed since suspend (expected {expected_hash[:12]}…, "
                    f"got {current_hash[:12]}…). The delta is no longer valid."
                )

        wasm_path = locate_wasm()

        saved_mounts = _parse_mounts(mounts) if mounts else meta.get("mounts", [])
        mount_dirs = [m["host_path"] for m in saved_mounts]
        store, exports = load_component(wasm_path, snapshot_path, mount_dirs or None)

        mount_entries = []
        for i, m in enumerate(saved_mounts):
            entry = object.__new__(type("MountEntry", (), {}))
            object.__setattr__(entry, "host-alias", f"mount{i}")
            object.__setattr__(entry, "guest-path", m["guest_path"])
            object.__setattr__(entry, "writable", m["writable"])
            mount_entries.append(entry)

        snap_rel = "snap/" + snapshot_path.name
        result = exports["session-resume"](
            snap_rel, delta_rel, _DEFAULT_SHELL, _DEFAULT_PROMPT, mount_entries
        )
        session_id = int(_unwrap_result(result))

        instance = cls.__new__(cls)
        instance._snapshot_path = snap_rel
        instance._mounts = saved_mounts
        instance._store = store
        instance._exports = exports
        instance._shell_session_id = session_id
        instance._in_context = True
        instance.commands = Commands(exports, snap_rel, instance._get_shell_session_id)
        instance.code = Code(exports, snap_rel, instance._get_code_session_id)

        cls._update_manifest(instance_id, "RUNNING")
        return instance

    @staticmethod
    def destroy(instance_id: str) -> None:
        """Remove a suspended instance from disk."""
        import shutil
        instance_dir = INSTANCES_DIR / instance_id
        if instance_dir.exists():
            shutil.rmtree(instance_dir)
        Sandbox._remove_from_manifest(instance_id)

    @staticmethod
    def list_instances() -> list[dict]:
        """List all suspended/running instances."""
        manifest_path = INSTANCES_DIR / "manifest.json"
        if not manifest_path.exists():
            return []
        return json.loads(manifest_path.read_text())

    @staticmethod
    def _update_manifest(instance_id: str, state: str) -> None:
        INSTANCES_DIR.mkdir(parents=True, exist_ok=True)
        manifest_path = INSTANCES_DIR / "manifest.json"
        entries = json.loads(manifest_path.read_text()) if manifest_path.exists() else []
        entries = [e for e in entries if e["id"] != instance_id]
        entries.append({"id": instance_id, "state": state})
        manifest_path.write_text(json.dumps(entries, indent=2))

    @staticmethod
    def _remove_from_manifest(instance_id: str) -> None:
        manifest_path = INSTANCES_DIR / "manifest.json"
        if not manifest_path.exists():
            return
        entries = json.loads(manifest_path.read_text())
        entries = [e for e in entries if e["id"] != instance_id]
        manifest_path.write_text(json.dumps(entries, indent=2))
