/**
 * The Worker the emulator runs in.
 */

import { mountSnapshot } from "../shims/filesystem.js";
import { pollStats } from "../shims/io.js";
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

async function handle(call: WorkerCall): Promise<unknown> {
    switch (call.kind) {
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
            return requireExecutor().sessionStart(
                call.snapshotPath,
                call.command,
                call.prompt,
                [],
            );

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
