import json
import os
import re
import uuid
from os.path import abspath
from pathlib import Path
from typing import Optional

from . import engines, snapshots
from .snapshots import cache_dir
from ._component import (
    _maybe_upgrade_tier,
    _note_degraded,
    active_tier,
    load_component,
    load_image_component,
    locate_wasm,
)
from ._result import unwrap_result as _unwrap_result
from .code import Code
from .commands import Commands
from .trace import TraceRecorder, trace_options

INSTANCES_DIR = Path.home() / ".vpod" / "instances"


def _parse_env(env: dict[str, str]) -> dict[str, str]:
    """The guest receives these as a shell `export`, so a name that is not a
    plain identifier could carry arbitrary shell along with it."""
    parsed = {}

    for name, value in env.items():
        if not _ENV_NAME.fullmatch(name):
            raise ValueError(
                f"environment variable name {name!r} is not a plain identifier"
            )
        parsed[name] = str(value)

    return parsed


def _env_entries(env: dict[str, str]) -> list:
    entries = []

    for name, value in env.items():
        entry = object.__new__(type("EnvVar", (), {}))
        object.__setattr__(entry, "name", name)
        object.__setattr__(entry, "value", value)
        entries.append(entry)

    return entries


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

_ENV_NAME = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")

_DEFAULT_SHELL = "/bin/sh"
_DEFAULT_PROMPT = "# "


class Sandbox:

    def __init__(
        self,
        snapshot: str = "alpine:latest",
        mounts: dict[str, str] | None = None,
        env: dict[str, str] | None = None,
        registry_url: str | None = None,
        api_key: str | None = None,
        engine: str = "auto",
        trace=None,
    ):
        if engine not in engines.ENGINE_MODES:
            raise ValueError(f"engine must be one of {engines.ENGINE_MODES}, got {engine!r}")
        self.trace = TraceRecorder(
            trace_options(trace), lambda: (self._exports, self._shell_session_id)
        )

        pulled = snapshots._pull(snapshot, registry_url, api_key, engine_mode=engine)
        snapshot_path = pulled.path

        self._snapshot_path = "snap/" + snapshot_path.name
        self._snapshot_file = snapshot_path
        self._mounts = _parse_mounts(mounts) if mounts else []
        self._env = _parse_env(env) if env else {}
        self._shell_session_id: Optional[int] = None
        self._in_context = False
        self._migrating = False
        self._image_engine: dict | None = None

        mount_dirs = [m["host_path"] for m in self._mounts] or None
        chosen = None
        if engine == "auto" and pulled.entry is not None:
            chosen = engines.select(pulled.entry, engines.bundled_interface())
            engines.announce(pulled.entry, chosen)

        started_on_image_engine = chosen is not None and self._start_on_image_engine(
            chosen, snapshot_path, mount_dirs
        )
        if not started_on_image_engine:
            if chosen is not None:
                engines.prepare_in_background(
                    pulled.entry["id"], chosen, pulled.registry_url, pulled.api_key
                )
            self._store, self._exports = load_component(
                locate_wasm(), snapshot_path, mount_dirs, upgrade_to_aot=chosen is None
            )
            self._tier = active_tier()

        self.trace._require_support(self._exports)

        self.commands = Commands(
            lambda: self._exports,
            self._snapshot_path,
            self._get_shell_session_id,
            self.trace,
        )

        self.code = Code(
            lambda: self._exports,
            self._snapshot_path,
            self._get_code_session_id,
            self.trace,
        )

    def _start_on_image_engine(self, chosen: dict, snapshot_path: Path, mount_dirs) -> bool:
        """Start on the snapshot's own engine if it is compiled and loads."""
        cwasm_path = engines.compiled_path(chosen)
        if cwasm_path is None:
            return False
        try:
            self._store, self._exports = load_image_component(cwasm_path, snapshot_path, mount_dirs)
        except Exception as failure:
            _note_degraded("the snapshot's engine would not load, using the bundled engine", failure)
            cwasm_path.unlink(missing_ok=True)
            return False
        self._tier = "image"
        self._image_engine = chosen
        return True

    @classmethod
    def create(
        cls,
        snapshot: str = "vsnap-base:latest",
        mounts: dict[str, str] | None = None,
        env: dict[str, str] | None = None,
        registry_url: str | None = None,
        api_key: str | None = None,
        engine: str = "auto",
        trace=None,
    ) -> "Sandbox":
        return cls(
            snapshot,
            mounts=mounts,
            env=env,
            registry_url=registry_url,
            api_key=api_key,
            engine=engine,
            trace=trace,
        )

    @property
    def tier(self) -> str | None:
        """The engine this sandbox runs on: "image", "aot", or "base"."""
        return self._tier

    def _mount_entries(self) -> list:
        mount_entries = []
        for i, m in enumerate(self._mounts):
            entry = object.__new__(type("MountEntry", (), {}))
            object.__setattr__(entry, "host-alias", f"mount{i}")
            object.__setattr__(entry, "guest-path", m["guest_path"])
            object.__setattr__(entry, "writable", m["writable"])
            mount_entries.append(entry)
        return mount_entries

    def _env_entries(self) -> list:
        return _env_entries(self._env)

    def _get_shell_session_id(self) -> int:
        self._maybe_upgrade_engine()
        if self._shell_session_id is None:
            result = self._exports["session-start"](
                self._snapshot_path,
                _DEFAULT_SHELL,
                _DEFAULT_PROMPT,
                self._mount_entries(),
                self._env_entries(),
            )
            self._shell_session_id = int(_unwrap_result(result))
            self.trace._start(self._exports, self._shell_session_id)
        return self._shell_session_id

    def _maybe_upgrade_engine(self) -> None:
        """Hop this sandbox onto the AOT tier once its cache lands."""
        if self._tier != "base" or self._migrating:
            return

        _maybe_upgrade_tier()
        if active_tier() != "aot":
            return

        self._migrating = True
        try:
            mount_dirs = [m["host_path"] for m in self._mounts] or None

            if self._shell_session_id is None:
                try:
                    self._store, self._exports = load_component(
                        locate_wasm(), self._snapshot_file, mount_dirs
                    )
                except Exception:
                    return
            else:
                old_store, old_exports = self._store, self._exports
                try:
                    instance_id = self.suspend()
                except Exception:
                    return
                delta_rel = f"instances/{instance_id}/delta.bin"

                def _resume(exports) -> None:
                    result = exports["session-resume"](
                        self._snapshot_path, delta_rel, _DEFAULT_SHELL,
                        _DEFAULT_PROMPT, self._mount_entries(), self._env_entries(),
                    )
                    self._shell_session_id = int(_unwrap_result(result))
                    self.trace._start(exports, self._shell_session_id)

                try:
                    self._store, self._exports = load_component(
                        locate_wasm(), self._snapshot_file, mount_dirs
                    )
                    _resume(self._exports)
                except Exception:
                    self._store, self._exports = old_store, old_exports
                    _resume(old_exports)
                    Sandbox.destroy(instance_id)
                    return

                Sandbox.destroy(instance_id)

            self._tier = "aot"
        finally:
            self._migrating = False

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
            self.trace._drain()
            self._exports["session-close"](self._shell_session_id)
            self._shell_session_id = None
        self.trace._close()

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
        self.trace._drain()
        _unwrap_result(self._exports["session-suspend"](session_id, delta_rel))

        (instance_dir / "meta.json").write_text(json.dumps({
            "snapshot": self._snapshot_path,
            "snapshot_sha256": self._snapshot_sha256(),
            "mounts": self._mounts,
            "state": "SUSPENDED",
            "engine": self._image_engine,
        }))

        self._shell_session_id = None
        self._update_manifest(instance_id, "SUSPENDED")
        return instance_id

    @classmethod
    def resume(
        cls,
        instance_id: str,
        mounts: dict[str, str] | None = None,
        env: dict[str, str] | None = None,
        trace=None,
    ) -> "Sandbox":
        resumed_env = _parse_env(env) if env else {}
        options = trace_options(trace)
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

        saved_mounts = _parse_mounts(mounts) if mounts else meta.get("mounts", [])
        mount_dirs = [m["host_path"] for m in saved_mounts]

        image_engine = meta.get("engine")
        if image_engine:
            cwasm_path = engines.compiled_path(image_engine)
            if cwasm_path is None:
                raise RuntimeError(
                    f"Instance {instance_id} was suspended on its snapshot's own engine "
                    f"(vpod {image_engine.get('vpod_version')}), which is no longer "
                    f"compiled on this machine. Start a sandbox on that snapshot once so "
                    f"the engine is prepared again, then resume."
                )
            store, exports = load_image_component(cwasm_path, snapshot_path, mount_dirs or None)
            tier = "image"
        else:
            store, exports = load_component(locate_wasm(), snapshot_path, mount_dirs or None)
            tier = active_tier()

        instance = cls.__new__(cls)
        instance.trace = TraceRecorder(
            options, lambda: (instance._exports, instance._shell_session_id)
        )
        instance.trace._require_support(exports)

        mount_entries = []
        for i, m in enumerate(saved_mounts):
            entry = object.__new__(type("MountEntry", (), {}))
            object.__setattr__(entry, "host-alias", f"mount{i}")
            object.__setattr__(entry, "guest-path", m["guest_path"])
            object.__setattr__(entry, "writable", m["writable"])
            mount_entries.append(entry)

        snap_rel = "snap/" + snapshot_path.name
        result = exports["session-resume"](
            snap_rel,
            delta_rel,
            _DEFAULT_SHELL,
            _DEFAULT_PROMPT,
            mount_entries,
            _env_entries(resumed_env),
        )
        session_id = int(_unwrap_result(result))

        instance._env = resumed_env
        instance._snapshot_path = snap_rel
        instance._snapshot_file = snapshot_path
        instance._tier = tier
        instance._image_engine = image_engine or None
        instance._migrating = False
        instance._mounts = saved_mounts
        instance._store = store
        instance._exports = exports
        instance._shell_session_id = session_id
        instance._in_context = True
        instance.trace._start(exports, session_id)
        instance.commands = Commands(
            lambda: instance._exports, snap_rel, instance._get_shell_session_id, instance.trace
        )
        instance.code = Code(
            lambda: instance._exports, snap_rel, instance._get_code_session_id, instance.trace
        )

        Sandbox.destroy(instance_id)
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
        """List all suspended instances."""
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
