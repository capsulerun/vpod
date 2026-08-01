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

const DEFAULT_SHELL = "/bin/sh";
const DEFAULT_PROMPT = "# ";
const DEFAULT_SNAPSHOT = "vsnap-base:latest";
const DEFAULT_TIMEOUT_SECONDS = 120;
const TIMEOUT_EXIT_CODE = 124;

const PYTHON_PREFIX = String.fromCharCode(0);

export type SnapshotSource =
    | string
    | { path: string }
    | { bytes: ArrayBuffer | Uint8Array; name: string };

export interface SandboxOptions extends SandboxRuntimeOptions {
    snapshot?: SnapshotSource;
    network?: boolean;
    registryUrl?: string;
}

export interface RunOptions {
    timeout?: number;
}

export class Commands {
    #sandbox: Sandbox;

    constructor(sandbox: Sandbox) {
        this.#sandbox = sandbox;
    }

    async run(command: string, options: RunOptions = {}): Promise<CommandResult> {
        const result = await this.#sandbox._exec(command, options.timeout);
        return new CommandResult(
            normalizeLineEndings(result.stdout),
            normalizeLineEndings(result.stderr ?? ""),
            result.exitCode,
        );
    }
}

export class Code {
    #sandbox: Sandbox;

    constructor(sandbox: Sandbox) {
        this.#sandbox = sandbox;
    }

    async run(code: string, options: RunOptions = {}): Promise<CodeExecution> {
        const timeout = options.timeout ?? DEFAULT_TIMEOUT_SECONDS;
        const result = await this.#sandbox._exec(PYTHON_PREFIX + code, timeout);

        if (result.exitCode === TIMEOUT_EXIT_CODE) {
            const timedOut = parseCodeOutput(result.stdout);
            return new CodeExecution(
                timedOut.text,
                `Timed out after ${timeout}s`,
                timedOut.logs,
            );
        }

        return parseCodeOutput(result.stdout, result.stderr ?? "");
    }
}

export class Sandbox {
    readonly commands: Commands;
    readonly code: Code;

    readonly #runtime: SandboxRuntime;
    readonly #snapshotPath: string;
    readonly #snapshotId: string;
    #sessionHandle: bigint | null = null;

    private constructor(runtime: SandboxRuntime, snapshotPath: string, snapshotId: string) {
        this.#runtime = runtime;
        this.#snapshotPath = snapshotPath;
        this.#snapshotId = snapshotId;
        this.commands = new Commands(this);
        this.code = new Code(this);
    }

    static async #mount(
        runtime: SandboxRuntime,
        snapshot: SnapshotSource,
        registryUrl: string | undefined,
    ): Promise<{ snapshotPath: string; snapshotId: string }> {
        if (typeof snapshot === "string") {
            const pulled = await runtime.pullSnapshot(snapshot, { registryUrl });
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
    ): Promise<void> {
        if (runtime.networkBackend !== "none") {
            return; // Node already has real sockets.
        }

        if (requested === false) {
            return;
        }

        if (requested === undefined) {
            if (networkAvailability().available) {
                await runtime.enableNetwork();
            }
            return;
        }

        await runtime.enableNetwork();
    }

    static async create(options: SandboxOptions = {}): Promise<Sandbox> {
        const runtime = new SandboxRuntime(options);
        await runtime.ready();

        await Sandbox.#connectNetwork(runtime, options.network);

        const mounted = await Sandbox.#mount(
            runtime,
            options.snapshot ?? DEFAULT_SNAPSHOT,
            options.registryUrl,
        );
        return new Sandbox(runtime, mounted.snapshotPath, mounted.snapshotId);
    }

    get snapshotId(): string {
        return this.#snapshotId;
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

    async #ensureSession(): Promise<bigint> {
        this.#sessionHandle ??= await this.#runtime.sessionStart(
            this.#snapshotPath,
            DEFAULT_SHELL,
            DEFAULT_PROMPT,
        );
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
        return store.save(this.#snapshotId, delta);
    }

    static async resume(
        instance: string | SuspendedInstance,
        options: SandboxOptions = {},
    ): Promise<Sandbox> {
        const resolved =
            typeof instance === "string"
                ? await (await InstanceStore.open()).load(instance)
                : instance;

        const runtime = new SandboxRuntime(options);
        await runtime.ready();

        await Sandbox.#connectNetwork(runtime, options.network);

        const mounted = await Sandbox.#mount(
            runtime,
            options.snapshot ?? resolved.snapshotId,
            options.registryUrl,
        );

        const sandbox = new Sandbox(runtime, mounted.snapshotPath, mounted.snapshotId);
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
