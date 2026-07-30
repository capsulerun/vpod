export class CommandResult {
    readonly stdout: string;
    readonly stderr: string;
    readonly exitCode: number;

    constructor(stdout: string, stderr = "", exitCode = 0) {
        this.stdout = stdout;
        this.stderr = stderr;
        this.exitCode = exitCode;
    }

    get success(): boolean {
        return this.exitCode === 0;
    }
}

export class CodeExecution {
    readonly text: string;
    readonly error: string | null;
    readonly logs: string[];

    constructor(text: string, error: string | null = null, logs: string[] = []) {
        this.text = text;
        this.error = error;
        this.logs = logs;
    }

    get success(): boolean {
        return this.error === null;
    }
}

const ERROR_INDICATORS = [
    "Error",
    "Traceback",
    "not found",
    "error:",
    "syntax error",
];

export function normalizeLineEndings(value: string): string {
    return value.replace(/\r\n|\r/g, "\n");
}

function splitLines(value: string): string[] {
    const trimmed = normalizeLineEndings(value).trim();
    return trimmed.length === 0 ? [] : trimmed.split("\n");
}

export function parseCodeOutput(stdout: string, stderr = ""): CodeExecution {
    const logs = splitLines(stdout);
    const text = logs.join("\n");
    const candidates = [...logs, ...splitLines(stderr)];
    const errors = candidates.filter((line) =>
        ERROR_INDICATORS.some((indicator) => line.includes(indicator)),
    );

    if (errors.length > 0) {
        return new CodeExecution(text, errors[errors.length - 1]!, logs);
    }
    return new CodeExecution(text, null, logs);
}
