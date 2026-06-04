from typing import Optional

from . import snapshots
from ._component import load_component, locate_wasm
from ._result import unwrap_result as _unwrap_result
from .code import Code
from .commands import Commands

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

    def __init__(self, snapshot: str = "alpine:latest"):
        snapshot_path = snapshots.pull(snapshot)
        wasm_path = locate_wasm()

        self._snapshot_path = str(snapshot_path)
        self._store, self._exports = load_component(wasm_path, snapshot_path)
        self._session_id: Optional[int] = None

        self.commands = Commands(
            self._exports,
            self._snapshot_path,
            self._get_session_id,
        )

        self.code = Code(
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
            self._snapshot_path, _DEFAULT_SHELL, _DEFAULT_PROMPT
        )
        self._session_id = _unwrap_result(result)
        return self

    def __exit__(self, *_) -> None:
        self.code.close()
        if self._session_id is not None:
            self._exports["session-close"](self._session_id)
            self._session_id = None
