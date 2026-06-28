from os.path import abspath
from typing import Optional

from . import snapshots
from ._component import load_component, locate_wasm
from ._result import unwrap_result as _unwrap_result
from .code import Code
from .commands import Commands


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

        self._snapshot_path = snapshot_path.as_posix()
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
    def create(cls, snapshot: str = "alpine:latest", mounts: dict[str, str] | None = None) -> "Sandbox":
        return cls(snapshot, mounts=mounts)

    def _get_shell_session_id(self) -> int:
        if self._shell_session_id is None:
            mount_entries = []
            for m in self._mounts:
                entry = object.__new__(type("MountEntry", (), {}))
                object.__setattr__(entry, "host-alias", m["host_path"])
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
