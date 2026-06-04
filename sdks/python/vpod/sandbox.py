from typing import Optional

from . import snapshots
from ._component import load_component, locate_wasm
from .code import Code
from .commands import Commands

_DEFAULT_SHELL = "/bin/sh"
_DEFAULT_PROMPT = "$ "


class Sandbox:
    """
    Secure execution sandbox backed by a RISC-V VM.

    Stateless usage:
        sandbox = Sandbox.create()
        result = sandbox.commands.run("echo hello")

    Persistent session:
        with Sandbox.create() as sandbox:
            sandbox.commands.run("export DB=postgres://localhost/db")
            result = sandbox.commands.run("echo $DB")

            execution = sandbox.code.run("print(2 + 2)")
            print(execution.text)  # 4
    """

    def __init__(self, snapshot: str = "alpine:latest"):
        snapshot_path = snapshots.pull(snapshot)
        wasm_path = locate_wasm()

        self._snapshot_path = str(snapshot_path)
        self._store, self._exports = load_component(wasm_path)
        self._session_id: Optional[int] = None

        self.commands = Commands(
            self._store,
            self._exports,
            self._snapshot_path,
            self._get_session_id,
        )

        self.code = Code(
            self._store,
            self._exports,
            self._snapshot_path,
            self._get_session_id,
        )

    @classmethod
    def create(cls, snapshot: str = "alpine:latest") -> "Sandbox":
        return cls(snapshot)

    def _get_session_id(self) -> Optional[int]:
        return self._session_id

    def __enter__(self) -> "Sandbox":
        result = self._exports["session-start"](
            self._store, self._snapshot_path, _DEFAULT_SHELL, _DEFAULT_PROMPT
        )
        if result["is_err"]:
            raise RuntimeError(f"Failed to start session: {result['err']}")
        self._session_id = result["ok"]
        return self

    def __exit__(self, *_) -> None:
        if self._session_id is not None:
            self._exports["session-close"](self._store, self._session_id)
            self._session_id = None
