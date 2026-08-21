import threading
from typing import Callable

from ._result import unwrap_result
from .execution import CommandResult, normalize_line_endings

SLICE_NANOS = 100_000_000


class Commands:
    """Shell command execution interface for a sandbox."""

    def __init__(self, get_exports, snapshot_path: str, get_session_id):
        self._get_exports = get_exports
        self._snapshot_path = snapshot_path
        self._get_session_id = get_session_id
        self._interrupt_requested = threading.Event()

    def run(
        self,
        command: str,
        timeout: int = 120,
        on_stdout: Callable[[str], None] | None = None,
        on_stderr: Callable[[str], None] | None = None,
    ) -> CommandResult:
        session_id = self._get_session_id()
        exec_slice = self._get_exports()["session-exec-slice"]
        self._interrupt_requested.clear()

        code = command
        stopped = False
        stdout = ""
        stderr = ""

        while True:
            try:
                slice_output = unwrap_result(
                    exec_slice(session_id, code, timeout, SLICE_NANOS)
                )
            except KeyboardInterrupt:
                self._interrupt_requested.set()
                code = None
                continue

            code = None

            stdout_chunk = normalize_line_endings(slice_output.stdout)
            stderr_chunk = normalize_line_endings(slice_output.stderr or "")

            if stdout_chunk:
                stdout += stdout_chunk
                if on_stdout is not None:
                    on_stdout(stdout_chunk)
            if stderr_chunk:
                stderr += stderr_chunk
                if on_stderr is not None:
                    on_stderr(stderr_chunk)

            exit_code = getattr(slice_output, "exit-code")
            if exit_code is not None:
                return CommandResult(
                    stdout=stdout.rstrip(),
                    stderr=stderr.rstrip(),
                    exit_code=exit_code,
                )

            if stopped:
                continue

            if self._interrupt_requested.is_set():
                self._send_interrupt(session_id)
                stopped = True

    def interrupt(self) -> None:
        self._interrupt_requested.set()

    def _send_interrupt(self, session_id) -> None:
        unwrap_result(self._get_exports()["session-interrupt"](session_id))
