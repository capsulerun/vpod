from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Optional

if TYPE_CHECKING:
    from .trace import Trace


def normalize_line_endings(value: str) -> str:
    return value.replace("\r\n", "\n")


def split_lines(value: str) -> list[str]:
    trimmed = normalize_line_endings(value).strip()
    return trimmed.split("\n") if trimmed else []


def _require_trace(trace: Optional["Trace"]) -> "Trace":
    if trace is None:
        from .trace import NOT_ENABLED

        raise RuntimeError(NOT_ENABLED)
    return trace


@dataclass
class CommandResult:
    stdout: str
    stderr: str = ""
    exit_code: int = 0
    _trace: Optional["Trace"] = field(default=None, repr=False, compare=False)

    @property
    def success(self) -> bool:
        return self.exit_code == 0

    @property
    def trace(self) -> "Trace":
        return _require_trace(self._trace)


@dataclass
class CodeExecution:
    text: str
    error: Optional[str] = None
    logs: list[str] = field(default_factory=list)
    stderr: str = ""
    _trace: Optional["Trace"] = field(default=None, repr=False, compare=False)

    @property
    def success(self) -> bool:
        return self.error is None

    @property
    def trace(self) -> "Trace":
        return _require_trace(self._trace)
