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

        self._snapshot_path = snapshot_path.as_posix()
        self._store, self._exports = load_component(wasm_path, snapshot_path)
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
    def create(cls, snapshot: str = "alpine:latest") -> "Sandbox":
        return cls(snapshot)

    def _get_shell_session_id(self) -> int:
        if self._shell_session_id is None:
            result = self._exports["session-start"](
                self._snapshot_path, _DEFAULT_SHELL, _DEFAULT_PROMPT
            )
            self._shell_session_id = int(_unwrap_result(result))
        return self._shell_session_id

    def _get_code_session_id(self) -> Optional[int]:
        if not self._in_context:
            return None
        return self._get_shell_session_id()

    def __enter__(self) -> "Sandbox":
        self._in_context = True
        self.code._start_repl()

        return self

    def __exit__(self, *_) -> None:
        self.code.close()
        self._in_context = False
        if self._shell_session_id is not None:
            self._exports["session-close"](self._shell_session_id)
            self._shell_session_id = None

    def close(self) -> None:
        self.__exit__()
