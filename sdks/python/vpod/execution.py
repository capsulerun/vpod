from dataclasses import dataclass, field
from typing import Optional


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
