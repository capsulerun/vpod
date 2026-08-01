from dataclasses import dataclass, field
from typing import Optional


def normalize_line_endings(value: str) -> str:
    return value.replace("\r\n", "\n").replace("\r", "\n")


def split_lines(value: str) -> list[str]:
    trimmed = normalize_line_endings(value).strip()
    return trimmed.split("\n") if trimmed else []


@dataclass
class CommandResult:
    stdout: str
    stderr: str = ""
    exit_code: int = 0

    @property
    def success(self) -> bool:
        return self.exit_code == 0


@dataclass
class CodeExecution:
    text: str
    error: Optional[str] = None
    logs: list[str] = field(default_factory=list)

    @property
    def success(self) -> bool:
        return self.error is None
