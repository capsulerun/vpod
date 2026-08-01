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

        exit_code = getattr(result, "exit-code", 0)

        if exit_code == 124:
            return CodeExecution(
                text=output,
                error=f"Timed out after {timeout}s",
                logs=output.splitlines(),
            )

        return self._parse_output(output, stderr, exit_code)

    def close(self):
        pass

    def _parse_output(self, raw: str, stderr: str = "", exit_code: int = 0) -> CodeExecution:
        text = raw.strip()
        lines = text.splitlines()

        if exit_code == 0:
            return CodeExecution(text=text, logs=lines)

        # The traceback's last line is the exception, which is the useful message.
        from_stderr = [l for l in stderr.strip().splitlines() if l.strip()]
        spoken = from_stderr or [l for l in lines if l.strip()]

        return CodeExecution(
            text=text,
            error=spoken[-1] if spoken else f"exited {exit_code}",
            logs=lines,
        )
