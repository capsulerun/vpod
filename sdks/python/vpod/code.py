from ._result import unwrap_result
from .execution import CodeExecution, normalize_line_endings, split_lines



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

        exit_code = getattr(result, "exit-code", 0)

        if exit_code == 124:
            timed_out = self._parse_output(output)
            return CodeExecution(
                text=timed_out.text,
                error=f"Timed out after {timeout}s",
                logs=timed_out.logs,
            )

        return self._parse_output(output, stderr, exit_code)

    def close(self):
        pass

    def _parse_output(self, raw: str, stderr: str = "", exit_code: int = 0) -> CodeExecution:
        logs = split_lines(raw)
        text = "\n".join(logs)

        if exit_code == 0:
            return CodeExecution(text=text, logs=logs)

        spoken = [l for l in split_lines(stderr) if l.strip()]
        if not spoken and "Traceback (most recent call last):" in text:
            spoken = [l for l in logs if l.strip()]

        return CodeExecution(
            text=text,
            error=spoken[-1] if spoken else f"exited {exit_code}",
            logs=logs,
        )
