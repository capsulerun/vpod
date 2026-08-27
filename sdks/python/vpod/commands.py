import queue
import threading
from typing import Callable

from ._result import unwrap_result
from .execution import CommandResult, normalize_line_endings

SLICE_NANOS = 100_000_000

CLOSED, PIPED, TERMINAL = "closed", "piped", "terminal"


class Execution:
    """A command in flight."""

    def __init__(self, exports, session_id, command, timeout, mode):
        self._exports = exports
        self._session_id = session_id
        self._mode = mode
        self._tty = mode == TERMINAL
        self._timeout = timeout

        self._pending = command
        self._outbox = queue.Queue()
        self._interrupt_requested = threading.Event()
        self._interrupt_sent = False

        self.stdout = ""
        self.stderr = ""
        self.exit_code = None

    @property
    def done(self) -> bool:
        return self.exit_code is not None

    def write(self, data) -> None:
        """Queue input for the command's stdin. Safe from any thread."""
        self._outbox.put(data)


    def interrupt(self) -> None:
        self._interrupt_requested.set()

    def step(self) -> str:
        """Run one slice. Returns whatever the guest produced during it."""
        if self.done:
            return ""

        self._flush_input()

        slice_output = unwrap_result(
            self._exports["session-exec-slice"](
                self._session_id, self._pending, self._timeout, SLICE_NANOS, self._mode
            )
        )
        self._pending = None

        stdout_chunk = self._clean(slice_output.stdout)
        stderr_chunk = self._clean(slice_output.stderr or "")

        self.stdout += stdout_chunk
        self.stderr += stderr_chunk

        exit_code = getattr(slice_output, "exit-code")
        if exit_code is not None:
            self.exit_code = exit_code
        elif self._interrupt_requested.is_set() and not self._interrupt_sent:
            unwrap_result(self._exports["session-interrupt"](self._session_id))
            self._interrupt_sent = True

        return stdout_chunk + stderr_chunk if self._tty else stdout_chunk

    def __iter__(self):
        return self

    def __next__(self) -> str:
        while not self.done:
            try:
                chunk = self.step()
            except KeyboardInterrupt:
                self.interrupt()
                continue
            if chunk:
                return chunk
        raise StopIteration

    def wait(self) -> CommandResult:
        for _ in self:
            pass
        return self.result()

    def result(self) -> CommandResult:
        if not self.done:
            raise RuntimeError("the command is still running; call wait() first")

        if self._tty:
            return CommandResult(
                stdout=self.stdout, stderr=self.stderr, exit_code=self.exit_code
            )
        return CommandResult(
            stdout=self.stdout.rstrip(),
            stderr=self.stderr.rstrip(),
            exit_code=self.exit_code,
        )

    def _clean(self, chunk: str) -> str:
        return chunk if self._tty else normalize_line_endings(chunk)

    def _flush_input(self) -> None:
        buffered = bytearray()
        while True:
            try:
                item = self._outbox.get_nowait()
            except queue.Empty:
                break

            if item is None:
                continue
            if isinstance(item, str):
                item = item.encode()
            buffered.extend(item)

        if buffered:
            unwrap_result(self._exports["session-stdin"](self._session_id, bytes(buffered)))


class Commands:
    """Shell command execution interface for a sandbox."""

    def __init__(self, get_exports, snapshot_path: str, get_session_id):
        self._get_exports = get_exports
        self._snapshot_path = snapshot_path
        self._get_session_id = get_session_id
        self._running = None

    def start(
        self, command: str, timeout: int = 120, tty: bool = False, stdin_open: bool = False
    ) -> Execution:
        mode = TERMINAL if tty else (PIPED if stdin_open else CLOSED)
        execution = Execution(
            self._get_exports(), self._get_session_id(), command, timeout, mode
        )
        self._running = execution
        return execution

    def run(
        self,
        command: str,
        timeout: int = 120,
        on_stdout: Callable[[str], None] | None = None,
        on_stderr: Callable[[str], None] | None = None,
        stdin=None,
        tty: bool = False,
    ) -> CommandResult:
        execution = self.start(
            command, timeout=timeout, tty=tty, stdin_open=stdin is not None
        )

        if stdin is not None:
            self._feed(execution, stdin)

        while not execution.done:
            before_out = len(execution.stdout)
            before_err = len(execution.stderr)
            try:
                execution.step()
            except KeyboardInterrupt:
                execution.interrupt()
                continue

            if on_stdout is not None and len(execution.stdout) > before_out:
                on_stdout(execution.stdout[before_out:])
            if on_stderr is not None and len(execution.stderr) > before_err:
                on_stderr(execution.stderr[before_err:])

        return execution.result()

    def interrupt(self) -> None:
        if self._running is not None:
            self._running.interrupt()

    @staticmethod
    def _feed(execution: Execution, stdin) -> None:
        if isinstance(stdin, (str, bytes, bytearray)):
            execution.write(stdin)
            return

        if isinstance(stdin, queue.Queue):
            if execution._mode != TERMINAL:
                raise ValueError(
                    "streaming stdin needs tty=True. Without it the command reads a "
                    "staged file, so anything queued after it starts cannot reach it "
                    "and the command waits for an EOF that never comes. Pass str or "
                    "bytes for finite input, or tty=True to stream."
                )
            execution._outbox = stdin
            return

        raise TypeError(
            "stdin must be str, bytes, or a queue.Queue for interactive input"
        )
