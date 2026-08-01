/**
 * Guest runs on a `worker_threads` worker, so the calling thread stays responsive while a command runs.
 */

import { Worker } from "node:worker_threads";

import type { ExecutorTransport } from "../transport/types.js";
import type { NodeTransportOptions } from "./transport.js";
import type { WorkerCall } from "../worker/protocol.js";

interface PendingCall {
    resolve(value: unknown): void;
    reject(reason: Error): void;
}

type Reply =
    | { kind: "ready"; componentLoadMilliseconds: number }
    | { id: number; ok: true; value: unknown }
    | { id: number; ok: false; error: string };

class NodeWorkerTransport implements ExecutorTransport {
    readonly networkBackend = "sockets" as const;

    readonly #worker: Worker;
    readonly #pending = new Map<number, PendingCall>();
    readonly #ready: Promise<number>;

    #nextId = 1;
    #loaded = false;
    #resolveReady!: (milliseconds: number) => void;
    #rejectReady!: (reason: Error) => void;

    constructor(workerUrl: URL, options: NodeTransportOptions) {
        this.#ready = new Promise<number>((resolve, reject) => {
            this.#resolveReady = resolve;
            this.#rejectReady = reject;
        });

        this.#worker = new Worker(workerUrl, {
            workerData: { cacheDirectory: options.cacheDirectory, componentUrl: undefined },
        });

        this.#worker.on("message", (message: Reply) => this.#receive(message));
        this.#worker.on("error", (error: Error) => this.#failAll(error));
        this.#worker.on("exit", (code) => {
            if (code !== 0) {
                this.#failAll(new Error(`vpod: the guest worker exited with code ${code}`));
            }
        });
    }

    #updateRef(): void {
        if (this.#loaded && this.#pending.size === 0) {
            this.#worker.unref();
            return;
        }
        this.#worker.ref();
    }

    #receive(message: Reply): void {
        if ("kind" in message) {
            this.#loaded = true;
            this.#resolveReady(message.componentLoadMilliseconds);
            this.#updateRef();
            return;
        }

        const pending = this.#pending.get(message.id);
        this.#pending.delete(message.id);

        if (message.ok) {
            pending?.resolve(message.value);
            return;
        }
        pending?.reject(new Error(message.error));
    }

    #failAll(reason: Error): void {
        this.#rejectReady(reason);
        for (const pending of this.#pending.values()) {
            pending.reject(reason);
        }
        this.#pending.clear();
    }

    ready(): Promise<number> {
        return this.#ready;
    }

    call<T>(call: WorkerCall, transfer: Transferable[] = []): Promise<T> {
        const id = this.#nextId++;

        return new Promise<T>((resolve, reject) => {
            this.#pending.set(id, { resolve: resolve as (value: unknown) => void, reject });
            this.#updateRef();
            this.#worker.postMessage({ id, call }, transfer as never);
        }).finally(() => this.#updateRef()) as Promise<T>;
    }

    terminate(): void {
        this.#failAll(new Error("vpod: the runtime was terminated"));
        void this.#worker.terminate();
    }
}

export async function createNodeWorkerTransport(
    options: NodeTransportOptions = {},
): Promise<ExecutorTransport> {
    const workerUrl = new URL("./worker-entry.js", import.meta.url);
    const transport = new NodeWorkerTransport(workerUrl, options);

    await transport.ready();
    return transport;
}
