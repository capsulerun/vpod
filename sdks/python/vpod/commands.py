from ._result import unwrap_result
from .execution import CommandResult


class Commands:
    """Shell command execution interface for a sandbox."""

    def __init__(self, exports, snapshot_path: str, get_session_id):
        self._exports = exports
        self._snapshot_path = snapshot_path
        self._get_session_id = get_session_id

    def run(self, command: str, timeout: int = 120) -> CommandResult:
        session_id = self._get_session_id()
        exec = self._exports["session-exec"]
        result = unwrap_result(exec(session_id, command, timeout))

        return CommandResult(
            stdout=result.stdout,
            stderr=result.stderr or "",
            exit_code=getattr(result, "exit-code"),
        )
