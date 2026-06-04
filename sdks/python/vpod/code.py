from .execution import CodeExecution


class Code:
    """Code execution interface for a sandbox (interpreted languages)."""

    def __init__(self, store, exports, snapshot_path: str, get_session_id):
        self._store = store
        self._exports = exports
        self._snapshot_path = snapshot_path
        self._get_session_id = get_session_id

    def run(self, code: str, language: str = "python") -> CodeExecution:
        """
        Run code in the sandbox using the specified interpreter.
        Requires an active session.
        """
        session_id = self._get_session_id()

        if session_id is None:
            raise RuntimeError(
                "Code execution requires a session. "
                "Use 'with Sandbox.create() as sandbox:'"
            )

        result = self._exports["session-exec"](self._store, session_id, code)

        if result["is_err"]:
            return CodeExecution(text="", error=result["err"])

        output = result["ok"]
        return self._parse_output(output)

    def _parse_output(self, raw: str) -> CodeExecution:
        lines = raw.strip().splitlines()
        errors = [l for l in lines if "Error" in l or "Traceback" in l]

        if errors:
            return CodeExecution(text=raw, error=errors[-1], logs=lines)

        return CodeExecution(text=raw, logs=lines)
