from ._result import unwrap_result
from .execution import CodeExecution


class Code:
    """Code execution interface for a sandbox (interpreted languages)."""

    def __init__(self, exports, snapshot_path: str, get_session_id):
        self._exports = exports
        self._snapshot_path = snapshot_path
        self._get_session_id = get_session_id

    def run(self, code: str) -> CodeExecution:
        """Run Python code in the sandbox session. Requires an active session."""
        session_id = self._get_session_id()

        if session_id is None:
            raise RuntimeError(
                "Code execution requires a session. "
                "Use 'with Sandbox.create() as sandbox:'"
            )

        escaped = code.replace("'", "'\"'\"'")
        command = f"python3 -c '{escaped}'"
        output = unwrap_result(self._exports["session-exec"](session_id, command))
        return self._parse_output(output)

    def _parse_output(self, raw: str) -> CodeExecution:
        lines = raw.strip().splitlines()
        error_indicators = ("Error", "Traceback", "not found", "error:", "syntax error")
        errors = [l for l in lines if any(ind in l for ind in error_indicators)]

        if errors:
            return CodeExecution(text=raw, error=errors[-1], logs=lines)

        return CodeExecution(text=raw, logs=lines)
