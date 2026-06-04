from ._result import unwrap_result
from .execution import CommandResult


class Commands:
    """Shell command execution interface for a sandbox."""

    def __init__(self, exports, snapshot_path: str, get_session_id):
        self._exports = exports
        self._snapshot_path = snapshot_path
        self._get_session_id = get_session_id

    def run(self, command: str) -> CommandResult:
        session_id = self._get_session_id()

        if session_id is not None:
            return self._run_in_session(session_id, command)

        return self._run_stateless(command)

    def _run_in_session(self, session_id: int, command: str) -> CommandResult:
        output = unwrap_result(self._exports["session-exec"](session_id, command))
        return CommandResult(stdout=output)

    def _run_stateless(self, command: str) -> CommandResult:
        result = unwrap_result(self._exports["execute"](self._snapshot_path, command))
        return CommandResult(
            stdout=result.stdout,
            stderr=result.stderr or "",
            exit_code=getattr(result, "exit-code"),
        )
