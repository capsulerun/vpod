from ._result import unwrap_result
from .execution import CodeExecution


class Code:
    """Code execution interface for a sandbox — persistent Python REPL."""

    def __init__(self, get_exports, snapshot_path: str, get_session_id):
        self._get_exports = get_exports
        self._snapshot_path = snapshot_path
        self._get_session_id = get_session_id

    def run(self, code: str, timeout: int = 120) -> CodeExecution:
        """Run Python code in a persistent REPL. State lives in memory across calls."""
        session_id = self._get_session_id()
        if session_id is None:
            raise RuntimeError(
                "Code execution requires a session. "
                "Use 'with Sandbox.create() as sandbox:'"
            )

        result = unwrap_result(self._get_exports()["session-exec"](session_id, "\x00" + code, timeout))
        output = result.stdout if hasattr(result, 'stdout') else str(result)
        stderr = result.stderr if hasattr(result, 'stderr') else ""

        if getattr(result, "exit-code", 0) == 124:
            return CodeExecution(
                text=output,
                error=f"Timed out after {timeout}s",
                logs=output.splitlines(),
            )

        return self._parse_output(output, stderr)

    def close(self):
        pass

    def _parse_output(self, raw: str, stderr: str = "") -> CodeExecution:
        text = raw.strip()
        lines = text.splitlines()
        error_indicators = ("Error", "Traceback", "not found", "error:", "syntax error")

        all_lines = lines + stderr.strip().splitlines()
        errors = [l for l in all_lines if any(ind in l for ind in error_indicators)]

        if errors:
            return CodeExecution(text=text, error=errors[-1], logs=lines)

        return CodeExecution(text=text, logs=lines)
