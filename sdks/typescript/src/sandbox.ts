import {
    CodeExecution,
    CommandResult,
    normalizeLineEndings,
    parseCodeOutput,
} from "./execution.js";
import { SandboxRuntime, type SandboxRuntimeOptions } from "./runtime.js";
import { InstanceStore, type SuspendedInstance } from "./instances.js";
import type { NetworkCapabilities } from "./net/capabilities.js";
import { networkAvailability } from "./net/availability.js";
import { createDefaultTransport } from "./transport/default.js";
import { buildInfo, bundledEngineInterface, type BundledTier } from "./build-info.js";
import { checkApiKeyKind, resolveApiKey } from "./snapshots/auth.js";
import { fetchCatalogue, resolveSnapshot } from "./snapshots/catalogue.js";
import {
    announceEngine,
    coreModulesOf,
    decompressEngine,
    downloadEngineInBackground,
    readCachedEngine,
    selectEngine,
} from "./snapshots/engine.js";
import { defaultStore } from "./snapshots/index.js";
import { resolveRegistryUrl } from "./snapshots/registry.js";

const DEFAULT_SHELL = "/bin/sh";
const DEFAULT_PROMPT = "# ";
const DEFAULT_SNAPSHOT = "vsnap-base:latest";
const DEFAULT_TIMEOUT_SECONDS = 120;
const TIMEOUT_EXIT_CODE = 124;

const POWERED_OFF_EXIT_CODE = 256;
const POWERED_OFF_ERROR =
    "The guest powered off, so this sandbox has no machine left to run on. Create a new one.";

const PYTHON_PREFIX = String.fromCharCode(0);

export type SnapshotSource =
    | string
    | { path: string }
    | { bytes: ArrayBuffer | Uint8Array; name: string };

export type EngineMode = "auto" | "default";
export type SandboxTier = "image" | BundledTier;

export interface SandboxOptions extends SandboxRuntimeOptions {
    snapshot?: SnapshotSource;
    network?: boolean;
    registryUrl?: string;
    apiKey?: string;
    corsProxy?: string;
    /**
     * "auto" runs the snapshot on its own engine when it has one this SDK can use
     * and it is already cached; "default" always uses the bundled engine.
     */
    engine?: EngineMode;
}

interface ImageEngine {
    sha256: string;
    coreModules: Record<string, Uint8Array>;
}

const SLICE_NANOS = 100_000_000n;

const yieldToEventLoop = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

export type ExecMode = "closed" | "piped" | "terminal";

export type Stdin =
    | string
    | Uint8Array
    | AsyncIterable<string | Uint8Array>
    | ReadableStream<string | Uint8Array>;

export interface RunOptions {
    timeout?: number;
    signal?: AbortSignal;
    onStdout?: (chunk: string) => void;
    onStderr?: (chunk: string) => void;
    stdin?: Stdin;
    tty?: boolean;
}

const encoder = new TextEncoder();

/** Ctrl-D equivalent for ending the input */
const STREAM_EOF = new Uint8Array([0x04]);

const STREAM_ABANDONED = new Error(
    "vpod: the command ended before its input stream did, so the rest was not sent",
);

const isStreaming = (stdin: Stdin): boolean =>
    typeof stdin !== "string" && !(stdin instanceof Uint8Array);

function modeFor(options: RunOptions): ExecMode {
    if (options.stdin === undefined) return options.tty ? "terminal" : "closed";
    return options.tty || isStreaming(options.stdin) ? "terminal" : "piped";
}

const toBytes = (chunk: string | Uint8Array): Uint8Array =>
    typeof chunk === "string" ? encoder.encode(chunk) : chunk;

/** @internal The handle `run` drives. Not part of the public API. */
export class Execution {
    #runtime: SandboxRuntime;
    #handle: bigint;
    #pending: string | null;
    #timeout: bigint;
    #mode: ExecMode;
    #tty: boolean;

    #outbox: Uint8Array[] = [];
    #eofPending = false;
    #eofSent = false;
    #interruptRequested = false;
    #interruptSent = false;

    stdout = "";
    stderr = "";
    exitCode: number | null = null;

    /** @internal */
    constructor(
        runtime: SandboxRuntime,
        handle: bigint,
        command: string,
        timeoutSeconds: number,
        mode: ExecMode,
    ) {
        this.#runtime = runtime;
        this.#handle = handle;
        this.#pending = command;
        this.#timeout = BigInt(timeoutSeconds);
        this.#mode = mode;
        this.#tty = mode === "terminal";
    }

    get done(): boolean {
        return this.exitCode !== null;
    }

    /** How this command's stdin and streams are wired. */
    get mode(): ExecMode {
        return this.#mode;
    }

    write(data: string | Uint8Array): void {
        this.#outbox.push(toBytes(data));
    }

    /** @internal Marks the input source closed, so the command sees an end-of-file. */
    endInput(): void {
        this.#eofPending = true;
    }

    endInputNow(): void {
        if (!this.#tty || this.#eofSent) return;
        this.#eofSent = true;
        this.#outbox.push(STREAM_EOF);
    }

    interrupt(): void {
        this.#interruptRequested = true;
    }

    async step(): Promise<string> {
        if (this.done) return "";

        await this.#flushInput();

        const slice = await this.#runtime.sessionExecSlice(
            this.#handle,
            this.#pending,
            this.#timeout,
            SLICE_NANOS,
            this.#mode,
        );
        this.#pending = null;

        const stdoutChunk = this.#clean(slice.stdout);
        const stderrChunk = this.#clean(slice.stderr ?? "");

        this.stdout += stdoutChunk;
        this.stderr += stderrChunk;

        if (slice.exitCode != null) {
            this.exitCode = slice.exitCode;
        } else if (this.#interruptRequested && !this.#interruptSent) {
            await this.#runtime.sessionInterrupt(this.#handle);
            this.#interruptSent = true;
        }

        return this.#tty ? stdoutChunk + stderrChunk : stdoutChunk;
    }

    async *[Symbol.asyncIterator](): AsyncIterator<string> {
        while (!this.done) {
            const chunk = await this.step();
            if (chunk) yield chunk;
            await yieldToEventLoop();
        }
    }

    async wait(): Promise<CommandResult> {
        while (!this.done) {
            await this.step();
            await yieldToEventLoop();
        }
        return this.result();
    }

    result(): CommandResult {
        if (this.exitCode === null) {
            throw new Error("the command is still running; await wait() first");
        }
        return this.#tty
            ? new CommandResult(this.stdout, this.stderr, this.exitCode)
            : new CommandResult(this.stdout.trimEnd(), this.stderr.trimEnd(), this.exitCode);
    }

    #clean(chunk: string): string {
        return this.#tty ? chunk : normalizeLineEndings(chunk);
    }

    async #flushInput(): Promise<void> {
        if (this.#outbox.length === 0) {
            if (!this.#eofPending || !this.#tty || this.#eofSent || this.done) return;
            this.#eofPending = false;
            this.#eofSent = true;
            await this.#runtime.sessionStdin(this.#handle, STREAM_EOF);
            return;
        }

        const pending = this.#outbox;
        this.#outbox = [];

        const total = pending.reduce((n, part) => n + part.length, 0);
        const joined = new Uint8Array(total);
        let at = 0;
        for (const part of pending) {
            joined.set(part, at);
            at += part.length;
        }

        await this.#runtime.sessionStdin(this.#handle, joined);
    }
}

export class Commands {
    #sandbox: Sandbox;

    constructor(sandbox: Sandbox) {
        this.#sandbox = sandbox;
    }

    async #start(command: string, options: RunOptions): Promise<Execution> {
        const execution = await this.#sandbox._start(
            command,
            options.timeout ?? DEFAULT_TIMEOUT_SECONDS,
            modeFor(options),
        );
        this.#running = execution;
        return execution;
    }

    async run(command: string, options: RunOptions = {}): Promise<CommandResult> {
        if (options.stdin === undefined && !options.tty && !options.onStdout && !options.onStderr) {
            const result = await this.#sandbox._execSliced(
                command,
                options.timeout,
                options.signal,
                {},
            );
            return new CommandResult(
                normalizeLineEndings(result.stdout),
                normalizeLineEndings(result.stderr ?? ""),
                result.exitCode,
            );
        }

        options.signal?.throwIfAborted();
        const execution = await this.#start(command, options);

        const feeding =
            options.stdin === undefined ? null : feedStdin(execution, options.stdin);

        const onAbort = () => execution.interrupt();
        options.signal?.addEventListener("abort", onAbort, { once: true });

        try {
            let seenOut = 0;
            let seenErr = 0;
            while (!execution.done) {
                await execution.step();

                if (options.onStdout && execution.stdout.length > seenOut) {
                    options.onStdout(execution.stdout.slice(seenOut));
                    seenOut = execution.stdout.length;
                }
                if (options.onStderr && execution.stderr.length > seenErr) {
                    options.onStderr(execution.stderr.slice(seenErr));
                    seenErr = execution.stderr.length;
                }

                await yieldToEventLoop();
            }
        } finally {
            options.signal?.removeEventListener("abort", onAbort);
            feeding?.stop();
        }

        options.signal?.throwIfAborted();
        return execution.result();
    }

    async interrupt(): Promise<void> {
        if (this.#running && !this.#running.done) {
            this.#running.interrupt();
            return;
        }
        await this.#sandbox._interrupt();
    }

    #running: Execution | null = null;
}

function feedStdin(execution: Execution, stdin: Stdin): { stop(): void } {
    if (typeof stdin === "string" || stdin instanceof Uint8Array) {
        execution.write(stdin);
        execution.endInputNow();
        return { stop: () => {} };
    }

    let stopped = false;
    let cancel: (() => void) | null = null;

    const pump = (async () => {
        if (typeof (stdin as ReadableStream).getReader === "function") {
            const reader = (stdin as ReadableStream<string | Uint8Array>).getReader();
            cancel = () => void reader.cancel(STREAM_ABANDONED).catch(() => {});
            try {
                for (;;) {
                    const { done, value } = await reader.read();
                    if (done || stopped) break;
                    if (value !== undefined) execution.write(value);
                }
            } finally {
                reader.releaseLock();
            }
        } else {
            const iterator = (stdin as AsyncIterable<string | Uint8Array>)[Symbol.asyncIterator]();
            cancel = () => void iterator.return?.(STREAM_ABANDONED).catch?.(() => {});
            for (;;) {
                const { done, value } = await iterator.next();
                if (done || stopped) break;
                if (value !== undefined) execution.write(value);
            }
        }

        if (!stopped && !execution.done) execution.endInput();
    })();

    pump.catch(() => {});

    return {
        stop: () => {
            stopped = true;
            cancel?.();
        },
    };
}

export class Code {
    #sandbox: Sandbox;

    constructor(sandbox: Sandbox) {
        this.#sandbox = sandbox;
    }

    async run(code: string, options: RunOptions = {}): Promise<CodeExecution> {
        const timeout = options.timeout ?? DEFAULT_TIMEOUT_SECONDS;
        const result = await this.#sandbox._exec(PYTHON_PREFIX + code, timeout);

        if (result.exitCode === POWERED_OFF_EXIT_CODE) {
            const halted = parseCodeOutput(result.stdout, result.stderr ?? "");
            return new CodeExecution(
                halted.text,
                POWERED_OFF_ERROR,
                halted.logs,
                halted.stderr,
            );
        }

        if (result.exitCode === TIMEOUT_EXIT_CODE) {
            const timedOut = parseCodeOutput(result.stdout, result.stderr ?? "");
            return new CodeExecution(
                timedOut.text,
                `Timed out after ${timeout}s`,
                timedOut.logs,
                timedOut.stderr,
            );
        }

        return parseCodeOutput(result.stdout, result.stderr ?? "", result.exitCode);
    }
}

export class Sandbox {
    readonly commands: Commands;
    readonly code: Code;

    readonly #runtime: SandboxRuntime;
    readonly #snapshotPath: string;
    readonly #snapshotId: string;
    readonly #imageEngineSha256: string | null;
    #sessionHandle: bigint | null = null;

    private constructor(
        runtime: SandboxRuntime,
        snapshotPath: string,
        snapshotId: string,
        imageEngineSha256: string | null,
    ) {
        this.#runtime = runtime;
        this.#snapshotPath = snapshotPath;
        this.#snapshotId = snapshotId;
        this.#imageEngineSha256 = imageEngineSha256;
        this.commands = new Commands(this);
        this.code = new Code(this);
    }

    static async #withTransport(
        options: SandboxOptions,
        coreModules?: Record<string, Uint8Array>,
    ): Promise<SandboxOptions> {
        if (options.transport !== undefined) {
            return options;
        }

        const transport = await createDefaultTransport({ coreModules });
        if (transport !== undefined) {
            return { ...options, transport };
        }
        // Views into a downloaded component, so always over a plain ArrayBuffer.
        return coreModules === undefined
            ? options
            : { ...options, coreModules: coreModules as Record<string, BufferSource> };
    }

    static async #startRuntime(
        options: SandboxOptions,
        imageEngine: ImageEngine | null,
    ): Promise<{ runtime: SandboxRuntime; imageEngine: ImageEngine | null }> {
        if (imageEngine !== null) {
            let runtime: SandboxRuntime | undefined;
            try {
                runtime = new SandboxRuntime(
                    await Sandbox.#withTransport(options, imageEngine.coreModules),
                );
                await runtime.ready();
                return { runtime, imageEngine };
            } catch (thrown: unknown) {
                runtime?.terminate();
                console.warn(
                    `vpod: the snapshot's engine would not start, using the bundled engine. ${String(thrown)}`,
                );
            }
        }

        const runtime = new SandboxRuntime(await Sandbox.#withTransport(options));
        await runtime.ready();
        return { runtime, imageEngine: null };
    }

    static async #cachedImageEngine(
        options: SandboxOptions,
        snapshotName: string,
    ): Promise<ImageEngine | null> {
        const engineInterface = bundledEngineInterface();
        if (options.engine === "default" || options.transport !== undefined || engineInterface === null) {
            return null;
        }

        try {
            const store = await defaultStore();
            if (store === null) {
                return null;
            }

            const apiKey = resolveApiKey(options.apiKey);
            if (apiKey !== undefined) {
                checkApiKeyKind(apiKey);
            }
            const registryUrl = resolveRegistryUrl(options.registryUrl, apiKey);
            const catalogue = await fetchCatalogue(store, { registryUrl, apiKey });
            const entry = resolveSnapshot(catalogue.snapshots, snapshotName, registryUrl, apiKey !== undefined);

            const engine = selectEngine(entry, engineInterface);
            announceEngine(entry, engine, buildInfo.version);
            if (engine === null) {
                return null;
            }

            const cached = await readCachedEngine(store, engine);
            if (cached === null) {
                void downloadEngineInBackground(store, entry.id, engine, { registryUrl, apiKey });
                return null;
            }
            return { sha256: engine.sha256, coreModules: coreModulesOf(await decompressEngine(cached)) };
        } catch (thrown: unknown) {
            console.warn(`vpod: could not look for the snapshot's own engine. ${String(thrown)}`);
            return null;
        }
    }

    static async #recordedImageEngine(engineSha256: string): Promise<ImageEngine> {
        const store = await defaultStore();
        const cached =
            store === null
                ? null
                : await readCachedEngine(store, { sha256: engineSha256 } as Parameters<typeof readCachedEngine>[1]);

        if (cached === null) {
            throw new Error(
                `vpod: this instance was suspended on its snapshot's own engine ` +
                    `(${engineSha256.slice(0, 12)}), which is no longer cached here. Create a ` +
                    `sandbox on that snapshot once so the engine is downloaded again, then resume.`,
            );
        }
        return { sha256: engineSha256, coreModules: coreModulesOf(await decompressEngine(cached)) };
    }

    static async #mount(
        runtime: SandboxRuntime,
        snapshot: SnapshotSource,
        registryUrl: string | undefined,
        apiKey?: string,
    ): Promise<{ snapshotPath: string; snapshotId: string }> {
        if (typeof snapshot === "string") {
            const pulled = await runtime.pullSnapshot(snapshot, { registryUrl, apiKey });
            return { snapshotPath: pulled.snapshotPath, snapshotId: pulled.id };
        }

        if ("path" in snapshot) {
            const id = snapshot.path.split("/").pop()?.replace(/\.snap$/, "");
            return { snapshotPath: snapshot.path, snapshotId: id ?? "local" };
        }

        const bytes =
            snapshot.bytes instanceof Uint8Array ? snapshot.bytes.slice().buffer : snapshot.bytes;
        const mounted = await runtime.mountSnapshot(snapshot.name, bytes);
        return {
            snapshotPath: mounted.snapshotPath,
            snapshotId: snapshot.name.replace(/\.snap$/, ""),
        };
    }

    static async #connectNetwork(
        runtime: SandboxRuntime,
        requested: boolean | undefined,
        corsProxy: string | undefined,
    ): Promise<void> {
        if (runtime.networkBackend !== "none") {
            return;
        }

        if (requested === false) {
            return;
        }

        if (requested === undefined) {
            if (networkAvailability().available) {
                await runtime.enableNetwork({ corsProxy });
            }
            return;
        }

        await runtime.enableNetwork({ corsProxy });
    }

    static async create(options: SandboxOptions = {}): Promise<Sandbox> {
        if (options.engine !== undefined && options.engine !== "auto" && options.engine !== "default") {
            throw new Error(`vpod: engine must be "auto" or "default", got ${JSON.stringify(options.engine)}`);
        }

        const snapshot = options.snapshot ?? DEFAULT_SNAPSHOT;
        const cachedEngine =
            typeof snapshot === "string" ? await Sandbox.#cachedImageEngine(options, snapshot) : null;
        const { runtime, imageEngine } = await Sandbox.#startRuntime(options, cachedEngine);

        await Sandbox.#connectNetwork(runtime, options.network, options.corsProxy);

        const mounted = await Sandbox.#mount(
            runtime,
            snapshot,
            options.registryUrl,
            options.apiKey,
        );
        return new Sandbox(runtime, mounted.snapshotPath, mounted.snapshotId, imageEngine?.sha256 ?? null);
    }

    get snapshotId(): string {
        return this.#snapshotId;
    }

    /** The engine this sandbox runs on: "image", "aot", or "base"; null when the build did not record it. */
    get tier(): SandboxTier | null {
        return this.#imageEngineSha256 !== null ? "image" : buildInfo.bundledTier;
    }

    get network(): NetworkCapabilities {
        return this.#runtime.networkCapabilities();
    }

    get runtime(): SandboxRuntime {
        return this.#runtime;
    }

    /** @internal */
    async _exec(payload: string, timeoutSeconds = DEFAULT_TIMEOUT_SECONDS) {
        const handle = await this.#ensureSession();
        return this.#runtime.sessionExec(handle, payload, BigInt(timeoutSeconds));
    }

    /** @internal */
    async _start(command: string, timeoutSeconds: number, mode: ExecMode): Promise<Execution> {
        const handle = await this.#ensureSession();
        return new Execution(this.#runtime, handle, command, timeoutSeconds, mode);
    }

    async _execSliced(
        payload: string,
        timeoutSeconds = DEFAULT_TIMEOUT_SECONDS,
        signal?: AbortSignal,
        listeners: {
            onStdout?: (chunk: string) => void;
            onStderr?: (chunk: string) => void;
        } = {},
    ) {
        signal?.throwIfAborted();
        const handle = await this.#ensureSession();

        let code: string | null = payload;
        let stopped = false;
        let stdout = "";
        let stderr = "";

        for (;;) {
            const slice = await this.#runtime.sessionExecSlice(
                handle,
                code,
                BigInt(timeoutSeconds),
                SLICE_NANOS,
            );
            code = null;

            const stdoutChunk = normalizeLineEndings(slice.stdout);
            const stderrChunk = normalizeLineEndings(slice.stderr);

            if (stdoutChunk) {
                stdout += stdoutChunk;
                listeners.onStdout?.(stdoutChunk);
            }
            if (stderrChunk) {
                stderr += stderrChunk;
                listeners.onStderr?.(stderrChunk);
            }

            if (slice.exitCode != null) {
                if (stopped) {
                    signal?.throwIfAborted();
                }
                return {
                    stdout: stdout.trimEnd(),
                    stderr: stderr.trimEnd(),
                    exitCode: slice.exitCode,
                };
            }

            await yieldToEventLoop();

            if (!stopped && signal?.aborted) {
                await this.#runtime.sessionInterrupt(handle);
                stopped = true;
            }
        }
    }

    async _interrupt(): Promise<void> {
        if (this.#sessionHandle === null) {
            return;
        }
        await this.#runtime.sessionInterrupt(this.#sessionHandle);
    }

    async #ensureSession(): Promise<bigint> {
        if (this.#sessionHandle === null) {
            this.#sessionHandle = await this.#runtime.sessionStart(
                this.#snapshotPath,
                DEFAULT_SHELL,
                DEFAULT_PROMPT,
            );
        }
        return this.#sessionHandle;
    }

    async suspend(): Promise<Uint8Array> {
        const handle = await this.#ensureSession();
        const suspended = await this.#runtime.sessionSuspend(handle);
        this.#sessionHandle = null;
        return new Uint8Array(suspended.deltaBytes);
    }

    async suspendToOpfs(): Promise<string> {
        const delta = await this.suspend();
        const store = await InstanceStore.open();
        return store.save(this.#snapshotId, delta, this.#imageEngineSha256 ?? undefined);
    }

    static async resume(
        instance: string | SuspendedInstance,
        options: SandboxOptions = {},
    ): Promise<Sandbox> {
        const resolved =
            typeof instance === "string"
                ? await (await InstanceStore.open()).load(instance)
                : instance;

        const snapshot = options.snapshot ?? resolved.snapshotId;
        let wanted: ImageEngine | null;
        if (resolved.engineSha256 !== undefined) {
            wanted = await Sandbox.#recordedImageEngine(resolved.engineSha256);
        } else if (typeof instance === "string") {
            wanted = null;
        } else {
            wanted = typeof snapshot === "string" ? await Sandbox.#cachedImageEngine(options, snapshot) : null;
        }

        const { runtime, imageEngine } = await Sandbox.#startRuntime(options, wanted);
        if (resolved.engineSha256 !== undefined && imageEngine === null) {
            runtime.terminate();
            throw new Error(
                `vpod: this instance was suspended on its snapshot's own engine, which would not ` +
                    `start here, and its delta cannot resume on the bundled engine.`,
            );
        }

        await Sandbox.#connectNetwork(runtime, options.network, options.corsProxy);

        const mounted = await Sandbox.#mount(
            runtime,
            snapshot,
            options.registryUrl,
            options.apiKey,
        );

        const sandbox = new Sandbox(runtime, mounted.snapshotPath, mounted.snapshotId, imageEngine?.sha256 ?? null);
        const delta = resolved.delta.slice();
        sandbox.#sessionHandle = await runtime.sessionResume(
            mounted.snapshotPath,
            delta.buffer,
            DEFAULT_SHELL,
            DEFAULT_PROMPT,
        );

        if (typeof instance === "string") {
            await Sandbox.destroy(instance);
        }
        return sandbox;
    }

    static async listInstances(): Promise<{ id: string; snapshotId: string; savedAt: number }[]> {
        const store = await InstanceStore.open();
        return store.list();
    }

    static async destroy(instanceId: string): Promise<void> {
        const store = await InstanceStore.open();
        await store.remove(instanceId);
    }

    async close(): Promise<void> {
        if (this.#sessionHandle !== null) {
            await this.#runtime.sessionClose(this.#sessionHandle);
            this.#sessionHandle = null;
        }
        this.#runtime.terminate();
    }

    async [Symbol.asyncDispose](): Promise<void> {
        await this.close();
    }
}
