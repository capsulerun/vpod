from ._result import unwrap_result
from .execution import CodeExecution

_PYTHON_CMD = "python3"
_PYTHON_PROMPT = ">>> "


class Code:
    """Code execution interface for a sandbox — persistent Python REPL."""

    def __init__(self, exports, snapshot_path: str, get_session_id):
        self._exports = exports
        self._snapshot_path = snapshot_path
        self._get_session_id = get_session_id
        self._repl_session_id: int | None = None

    def run(self, code: str) -> CodeExecution:
        """Run Python code in a persistent REPL. State lives in memory across calls."""
        if self._get_session_id() is None:
            raise RuntimeError(
                "Code execution requires a session. "
                "Use 'with Sandbox.create() as sandbox:'"
            )

        if self._repl_session_id is None:
            self._start_repl()

        command = f"exec({repr(code)})"
        result = unwrap_result(
            self._exports["session-exec"](self._repl_session_id, command)
        )
        output = result.stdout if hasattr(result, 'stdout') else str(result)
        stderr = result.stderr if hasattr(result, 'stderr') else ""
        return self._parse_output(output, stderr)

    def _start_repl(self):
        result = self._exports["session-start"](
            self._snapshot_path, _PYTHON_CMD, _PYTHON_PROMPT
        )
        self._repl_session_id = int(unwrap_result(result))

    def close(self):
        if self._repl_session_id is not None:
            self._exports["session-close"](self._repl_session_id)
            self._repl_session_id = None

    def _parse_output(self, raw: str, stderr: str = "") -> CodeExecution:
        text = raw.strip()
        lines = text.splitlines()
        error_indicators = ("Error", "Traceback", "not found", "error:", "syntax error")

        all_lines = lines + stderr.strip().splitlines()
        errors = [l for l in all_lines if any(ind in l for ind in error_indicators)]

        if errors:
            return CodeExecution(text=text, error=errors[-1], logs=lines)

        return CodeExecution(text=text, logs=lines)
