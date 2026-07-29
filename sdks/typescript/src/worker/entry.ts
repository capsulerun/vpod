/**
 * The Worker the emulator runs in.
 */

import { mountSnapshot } from "../shims/filesystem.js";
import { pollStats } from "../shims/io.js";
import { evictById, pullSnapshot } from "../snapshots/pull.js";
import { SnapshotStore } from "../snapshots/store.js";
import type {
    ExecutionResult,
    WorkerCall,
    WorkerInit,
    WorkerMessage,
    WorkerRequest,
} from "./protocol.js";

interface Executor {
    sessionStart(
        snapshotPath: string,
        command: string,
        prompt: string,
        mounts: never[],
    ): bigint;
    sessionExec(
        handle: bigint,
        code: string,
        timeout: bigint | undefined,
    ): ExecutionResult;
    sessionClose(handle: bigint): void;
    sessionSuspend(handle: bigint, deltaPath: string): bigint;
    sessionResume(
        snapshotPath: string,
        deltaPath: string,
        command: string,
        prompt: string,
        mounts: never[],
    ): bigint;
}

const workerScope = globalThis as unknown as {
    postMessage(message: WorkerMessage, transfer?: Transferable[]): void;
    addEventListener(
        type: "message",
        listener: (event: { data: WorkerInit | WorkerRequest }) => void,
    ): void;
};

let executor: Executor | null = null;
let componentLoadMilliseconds = 0;

async function loadComponent(componentUrl: string): Promise<void> {
    const startedAt = performance.now();
    const module = (await import(/* @vite-ignore */ componentUrl)) as {
        executor: Executor;
    };
    executor = module.executor;
    componentLoadMilliseconds = performance.now() - startedAt;

    workerScope.postMessage({ kind: "ready", componentLoadMilliseconds });
}

function requireExecutor(): Executor {
    if (executor === null) {
        throw new Error("vpod worker: component is not loaded yet");
    }
    return executor;
}

const mountedSnapshotIds = new Map<string, string>();

async function explainRejectedSnapshot(
    snapshotPath: string,
    thrown: unknown,
): Promise<unknown> {
    const message = String((thrown as { payload?: unknown })?.payload ?? thrown);
    const id = mountedSnapshotIds.get(snapshotPath);
    if (!message.includes("invalid snapshot magic") || id === undefined) {
        return thrown;
    }

    await evictById(id);
    return new Error(
        `vpod: the guest rejected snapshot '${id}' with "invalid snapshot magic". ` +
            `A snapshot compressed twice looks exactly like this, and its checksum ` +
            `still matches because the extra layer is what was served. The cached ` +
            `copy has been dropped so a retry refetches; if the registry itself is ` +
            `serving it double-framed, the retry fails identically.`,
    );
}

async function handle(call: WorkerCall): Promise<unknown> {
    switch (call.kind) {
        case "pull-snapshot": {
            const pulled = await pullSnapshot({
                name: call.name,
                registryUrl: call.registryUrl,
                force: call.force,
            });
            const snapshotPath = mountSnapshot(
                `${pulled.entry.id}.snap`,
                pulled.bytes,
            );
            mountedSnapshotIds.set(snapshotPath, pulled.entry.id);
            return {
                snapshotPath,
                id: pulled.entry.id,
                byteLength: pulled.bytes.byteLength,
                source: pulled.source,
                fetchMilliseconds: pulled.fetchMilliseconds,
                verifyMilliseconds: pulled.verifyMilliseconds,
                storeMilliseconds: pulled.storeMilliseconds,
            };
        }

        case "storage-quota":
            return SnapshotStore.quota();

        case "fetch-snapshot": {
            const response = await fetch(call.url);
            if (!response.ok) {
                throw new Error(
                    `vpod worker: snapshot fetch failed with ${response.status} for ${call.url}`,
                );
            }
            const name = call.name ?? call.url.split("/").pop()!;
            const bytes = new Uint8Array(await response.arrayBuffer());
            return {
                snapshotPath: mountSnapshot(name, bytes),
                byteLength: bytes.byteLength,
            };
        }

        case "mount-snapshot": {
            const bytes = new Uint8Array(call.bytes);
            return {
                snapshotPath: mountSnapshot(call.name, bytes),
                byteLength: bytes.byteLength,
            };
        }

        case "session-start":
            try {
                return requireExecutor().sessionStart(
                    call.snapshotPath,
                    call.command,
                    call.prompt,
                    [],
                );
            } catch (thrown: unknown) {
                throw await explainRejectedSnapshot(call.snapshotPath, thrown);
            }

        case "session-exec":
            return requireExecutor().sessionExec(
                call.handle,
                call.code,
                call.timeoutSeconds ?? undefined,
            );

        case "session-close":
            requireExecutor().sessionClose(call.handle);
            return null;

        case "session-suspend":
            return requireExecutor().sessionSuspend(call.handle, call.deltaPath);

        case "session-resume":
            return requireExecutor().sessionResume(
                call.snapshotPath,
                call.deltaPath,
                call.command,
                call.prompt,
                [],
            );

        case "poll-stats":
            return pollStats();

        case "component-load-milliseconds":
            return componentLoadMilliseconds;
    }
}

function describe(thrown: unknown): string {
    if (thrown instanceof Error) {
        return thrown.stack ?? thrown.message;
    }

    return String((thrown as { payload?: unknown })?.payload ?? thrown);
}

workerScope.addEventListener("message", (event) => {
    const message = event.data;

    if ("kind" in message && message.kind === "init") {
        loadComponent(message.componentUrl).catch((thrown: unknown) => {
            workerScope.postMessage({ id: -1, ok: false, error: describe(thrown) });
        });
        return;
    }

    const request = message as WorkerRequest;
    void (async () => {
        try {
            const value = await handle(request.call);
            workerScope.postMessage({ id: request.id, ok: true, value });
        } catch (thrown: unknown) {
            workerScope.postMessage({
                id: request.id,
                ok: false,
                error: describe(thrown),
            });
        }
    })();
});
