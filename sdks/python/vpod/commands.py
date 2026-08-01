from ._result import unwrap_result
from .execution import CommandResult, normalize_line_endings


class Commands:
    """Shell command execution interface for a sandbox."""

    def __init__(self, get_exports, snapshot_path: str, get_session_id):
        self._get_exports = get_exports
        self._snapshot_path = snapshot_path
        self._get_session_id = get_session_id

    def run(self, command: str, timeout: int = 120) -> CommandResult:
        session_id = self._get_session_id()
        exec = self._get_exports()["session-exec"]
        result = unwrap_result(exec(session_id, command, timeout))

        return CommandResult(
            stdout=normalize_line_endings(result.stdout),
            stderr=normalize_line_endings(result.stderr or ""),
            exit_code=getattr(result, "exit-code"),
        )
