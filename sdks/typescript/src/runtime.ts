import type {
    ExecutionResult,
    PullResult,
    WorkerCall,
    WorkerMessage,
} from "./worker/protocol.js";

export interface SandboxRuntimeOptions {
    /**
     * Where the Worker script lives. Override when the package is served from a CDN or rebased by a bundler.
     */
    workerUrl?: string | URL;

    /** Where the jco-transpiled component entry point lives. */
    componentUrl?: string | URL;
}

export interface SnapshotMount {
    /** The path to hand to `sessionStart`. */
    snapshotPath: string;
    byteLength: number;
}

interface PendingCall {
    resolve(value: unknown): void;
    reject(reason: Error): void;
}

/**
 * Owns the Worker the emulator runs in and turns its message pairs into promises.
 */
export class SandboxRuntime {
    readonly #worker: Worker;
    readonly #pending = new Map<number, PendingCall>();

    #nextId = 1;
    #resolveReady!: (componentLoadMilliseconds: number) => void;
    #rejectReady!: (reason: Error) => void;

    readonly #ready: Promise<number>;

    constructor(options: SandboxRuntimeOptions = {}) {
        const workerUrl =
            options.workerUrl ?? new URL("./worker/entry.js", import.meta.url);
        const componentUrl =
            options.componentUrl ?? new URL("./component/vpod.js", import.meta.url);

        this.#ready = new Promise<number>((resolve, reject) => {
            this.#resolveReady = resolve;
            this.#rejectReady = reject;
        });

        this.#worker = new Worker(workerUrl, { type: "module" });
        this.#worker.addEventListener("message", (event: MessageEvent) => {
            this.#receive(event.data as WorkerMessage);
        });
        this.#worker.addEventListener("error", (event: ErrorEvent) => {
            this.#failAll(new Error(`vpod worker: ${event.message}`));
        });

        this.#worker.postMessage({
            kind: "init",
            componentUrl: new URL(componentUrl, self.location.href).href,
        });
    }

    #receive(message: WorkerMessage): void {
        if ("kind" in message) {
            this.#resolveReady(message.componentLoadMilliseconds);
            return;
        }

        if (!message.ok) {
            if (message.id === -1) {
                this.#rejectReady(new Error(message.error));
                return;
            }
            this.#pending.get(message.id)?.reject(new Error(message.error));
            this.#pending.delete(message.id);
            return;
        }

        this.#pending.get(message.id)?.resolve(message.value);
        this.#pending.delete(message.id);
    }

    #failAll(reason: Error): void {
        this.#rejectReady(reason);

        for (const pending of this.#pending.values()) {
            pending.reject(reason);
        }

        this.#pending.clear();
    }

    #call<T>(call: WorkerCall, transfer: Transferable[] = []): Promise<T> {
        const id = this.#nextId++;

        return new Promise<T>((resolve, reject) => {
            this.#pending.set(id, {
                resolve: resolve as (value: unknown) => void,
                reject,
            });

            this.#worker.postMessage({ id, call }, transfer);
        });
    }

    ready(): Promise<number> {
        return this.#ready;
    }

    /**
     * Resolve a snapshot through the registry, serving it from OPFS when it is already cached.
     */
    pullSnapshot(
        name?: string,
        options: { registryUrl?: string; force?: boolean } = {},
    ): Promise<PullResult> {
        return this.#call<PullResult>({
            kind: "pull-snapshot",
            name,
            registryUrl: options.registryUrl,
            force: options.force,
        });
    }

    storageQuota(): Promise<{ usage: number; quota: number } | null> {
        return this.#call({ kind: "storage-quota" });
    }

    fetchSnapshot(url: string, name?: string): Promise<SnapshotMount> {
        return this.#call<SnapshotMount>({ kind: "fetch-snapshot", url, name });
    }

    mountSnapshot(name: string, bytes: ArrayBuffer): Promise<SnapshotMount> {
        return this.#call<SnapshotMount>({ kind: "mount-snapshot", name, bytes }, [
            bytes,
        ]);
    }

    sessionStart(
        snapshotPath: string,
        command = "/bin/sh",
        prompt = "# ",
    ): Promise<bigint> {
        return this.#call<bigint>({
            kind: "session-start",
            snapshotPath,
            command,
            prompt,
        });
    }

    sessionExec(
        handle: bigint,
        code: string,
        timeoutSeconds: bigint | null = null,
    ): Promise<ExecutionResult> {
        return this.#call<ExecutionResult>({
            kind: "session-exec",
            handle,
            code,
            timeoutSeconds,
        });
    }

    sessionClose(handle: bigint): Promise<void> {
        return this.#call<void>({ kind: "session-close", handle });
    }

    pollStats(): Promise<{ spinCount: number; spinNanoseconds: number }> {
        return this.#call({ kind: "poll-stats" });
    }

    terminate(): void {
        this.#failAll(new Error("vpod runtime: terminated"));
        this.#worker.terminate();
    }
}
