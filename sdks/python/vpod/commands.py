from .execution import CommandResult


class Commands:
    """Shell command execution interface for a sandbox."""

    def __init__(self, store, exports, snapshot_path: str, get_session_id):
        self._store = store
        self._exports = exports
        self._snapshot_path = snapshot_path
        self._get_session_id = get_session_id

    def run(self, command: str) -> CommandResult:
        """
        Run a shell command in the sandbox.
        """
        session_id = self._get_session_id()

        if session_id is not None:
            return self._run_in_session(session_id, command)

        return self._run_stateless(command)

    def _run_in_session(self, session_id: int, command: str) -> CommandResult:
        result = self._exports["session-exec"](self._store, session_id, command)

        if result["is_err"]:
            return CommandResult(stdout="", stderr=result["err"], exit_code=1)

        return CommandResult(stdout=result["ok"])

    def _run_stateless(self, command: str) -> CommandResult:
        result = self._exports["execute"](self._store, self._snapshot_path, command)

        if result["is_err"]:
            return CommandResult(stdout="", stderr=result["err"], exit_code=1)

        r = result["ok"]
        return CommandResult(
            stdout=r["stdout"],
            stderr=r["stderr"] or "",
            exit_code=r["exit_code"],
        )
