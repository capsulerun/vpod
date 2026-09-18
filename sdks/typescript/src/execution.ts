import { TRACE_NOT_ENABLED, type Trace } from "./trace.js";

function requireTrace(trace: Trace | null): Trace {
    if (trace === null) {
        throw new Error(TRACE_NOT_ENABLED);
    }
    return trace;
}

export class CommandResult {
    readonly stdout: string;
    readonly stderr: string;
    readonly exitCode: number;
    readonly #trace: Trace | null;

    constructor(stdout: string, stderr = "", exitCode = 0, trace: Trace | null = null) {
        this.stdout = stdout;
        this.stderr = stderr;
        this.exitCode = exitCode;
        this.#trace = trace;
    }

    get success(): boolean {
        return this.exitCode === 0;
    }

    get trace(): Trace {
        return requireTrace(this.#trace);
    }
}

export class CodeExecution {
    readonly text: string;
    readonly error: string | null;
    readonly logs: string[];
    readonly stderr: string;
    readonly #trace: Trace | null;

    constructor(
        text: string,
        error: string | null = null,
        logs: string[] = [],
        stderr = "",
        trace: Trace | null = null,
    ) {
        this.text = text;
        this.error = error;
        this.logs = logs;
        this.stderr = stderr;
        this.#trace = trace;
    }

    get success(): boolean {
        return this.error === null;
    }

    get trace(): Trace {
        return requireTrace(this.#trace);
    }

    /** @internal */
    _withTrace(trace: Trace | null): CodeExecution {
        return new CodeExecution(this.text, this.error, this.logs, this.stderr, trace);
    }
}

export function normalizeLineEndings(value: string): string {
    return value.replace(/\r\n/g, "\n");
}

function splitLines(value: string): string[] {
    const trimmed = normalizeLineEndings(value).trim();
    return trimmed.length === 0 ? [] : trimmed.split("\n");
}

export function parseCodeOutput(stdout: string, stderr = "", exitCode = 0): CodeExecution {
    const logs = splitLines(stdout);
    const text = logs.join("\n");
    const diagnostics = normalizeLineEndings(stderr).trim();

    if (exitCode === 0) {
        return new CodeExecution(text, null, logs, diagnostics);
    }

    const said = (lines: string[]) => lines.filter((line) => line.trim() !== "");
    const spoken = said(splitLines(stderr));
    const chosen = spoken.length > 0 ? spoken : text.includes("Traceback (most recent call last):") ? said(logs) : [];

    return new CodeExecution(
        text,
        chosen[chosen.length - 1] ?? `exited ${exitCode}`,
        logs,
        diagnostics,
    );
}
