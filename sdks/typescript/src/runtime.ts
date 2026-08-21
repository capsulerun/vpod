import {
    WorkerTransport,
    type NetworkOptions,
    type WorkerTransportOptions,
} from "./transport/worker.js";
import { capabilitiesOf } from "./net/capabilities.js";
import type { NetworkBackendName, NetworkCapabilities } from "./net/capabilities.js";
import type { ExecutorTransport } from "./transport/types.js";
import type {
    ExecutionResult,
    PullResult,
    SliceOutput,
    SnapshotMount,
    SuspendResult,
} from "./worker/protocol.js";

export interface SandboxRuntimeOptions extends WorkerTransportOptions {
    transport?: ExecutorTransport;
}

export interface StorageQuota {
    usage: number;
    quota: number;
}

export class SandboxRuntime {
    readonly #transport: ExecutorTransport;
    #networkBackend: NetworkBackendName = "none";

    constructor(options: SandboxRuntimeOptions = {}) {
        this.#transport = options.transport ?? new WorkerTransport(options);
        this.#networkBackend = this.#transport.networkBackend ?? "none";
    }

    ready(): Promise<number> {
        return this.#transport.ready();
    }

    async enableNetwork(options: NetworkOptions = {}): Promise<void> {
        const transport = this.#transport;
        if (!(transport instanceof WorkerTransport)) {
            throw new Error(
                "vpod: networking needs the worker transport, which owns the thread " +
                    "that runs fetch. A custom transport has to provide its own.",
            );
        }

        await transport.enableNetwork(options);
        this.#networkBackend = "fetch";
    }

    get networkBackend(): NetworkBackendName {
        return this.#networkBackend;
    }

    networkCapabilities(): NetworkCapabilities {
        return capabilitiesOf(this.#networkBackend);
    }

    pullSnapshot(
        name?: string,
        options: { registryUrl?: string; force?: boolean } = {},
    ): Promise<PullResult> {
        return this.#transport.call<PullResult>({
            kind: "pull-snapshot",
            name,
            registryUrl: options.registryUrl,
            force: options.force,
        });
    }

    storageQuota(): Promise<StorageQuota | null> {
        return this.#transport.call<StorageQuota | null>({ kind: "storage-quota" });
    }

    fetchSnapshot(url: string, name?: string): Promise<SnapshotMount> {
        return this.#transport.call<SnapshotMount>({ kind: "fetch-snapshot", url, name });
    }

    mountSnapshot(name: string, bytes: ArrayBuffer): Promise<SnapshotMount> {
        return this.#transport.call<SnapshotMount>({ kind: "mount-snapshot", name, bytes }, [
            bytes,
        ]);
    }

    sessionStart(snapshotPath: string, command = "/bin/sh", prompt = "# "): Promise<bigint> {
        return this.#transport.call<bigint>({
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
        return this.#transport.call<ExecutionResult>({
            kind: "session-exec",
            handle,
            code,
            timeoutSeconds,
        });
    }

    sessionExecSlice(
        handle: bigint,
        code: string | null,
        timeoutSeconds: bigint | null,
        sliceNanos: bigint,
    ): Promise<SliceOutput> {
        return this.#transport.call<SliceOutput>({
            kind: "session-exec-slice",
            handle,
            code,
            timeoutSeconds,
            sliceNanos,
        });
    }

    sessionInterrupt(handle: bigint): Promise<void> {
        return this.#transport.call<void>({ kind: "session-interrupt", handle });
    }

    sessionClose(handle: bigint): Promise<void> {
        return this.#transport.call<void>({ kind: "session-close", handle });
    }

    sessionSuspend(handle: bigint): Promise<SuspendResult> {
        return this.#transport.call<SuspendResult>({ kind: "session-suspend", handle });
    }

    sessionResume(
        snapshotPath: string,
        deltaBytes: ArrayBuffer,
        command = "/bin/sh",
        prompt = "# ",
    ): Promise<bigint> {
        return this.#transport.call<bigint>(
            { kind: "session-resume", snapshotPath, deltaBytes, command, prompt },
            [deltaBytes],
        );
    }

    pollStats(): Promise<{ spinCount: number; spinNanoseconds: number }> {
        return this.#transport.call({ kind: "poll-stats" });
    }

    terminate(): void {
        this.#transport.terminate();
    }
}
