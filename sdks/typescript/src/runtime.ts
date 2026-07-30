import { WorkerTransport, type WorkerTransportOptions } from "./transport/worker.js";
import type { ExecutorTransport } from "./transport/types.js";
import type {
    ExecutionResult,
    PullResult,
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

    constructor(options: SandboxRuntimeOptions = {}) {
        this.#transport = options.transport ?? new WorkerTransport(options);
    }

    ready(): Promise<number> {
        return this.#transport.ready();
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
