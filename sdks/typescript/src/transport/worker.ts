import { assetUrl } from "../asset-base.js";
import { requireNetwork } from "../net/availability.js";
import type { DriverOptions } from "../net/driver-protocol.js";
import type { WorkerCall, WorkerMessage } from "../worker/protocol.js";
import type { ExecutorTransport } from "./types.js";

export interface WorkerTransportOptions {
    workerUrl?: string | URL;
    componentUrl?: string | URL;
    networkWorkerUrl?: string | URL;
}

export interface NetworkOptions extends DriverOptions {
    allowedPorts?: number[];
}

interface PendingCall {
    resolve(value: unknown): void;
    reject(reason: Error): void;
}

export class WorkerTransport implements ExecutorTransport {
    readonly #worker: Worker;
    readonly #pending = new Map<number, PendingCall>();
    readonly #ready: Promise<number>;
    readonly #networkWorkerUrl: string | URL;

    #networkWorker: Worker | undefined;
    #nextId = 1;
    #resolveReady!: (componentLoadMilliseconds: number) => void;
    #rejectReady!: (reason: Error) => void;

    constructor(options: WorkerTransportOptions = {}) {
        const workerUrl =
            options.workerUrl ?? assetUrl("worker/entry.js");
        const componentUrl =
            options.componentUrl ?? assetUrl("component/vpod.js");

        this.#networkWorkerUrl = options.networkWorkerUrl ?? assetUrl("net/entry.js");

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

    ready(): Promise<number> {
        return this.#ready;
    }

    async enableNetwork(options: NetworkOptions = {}): Promise<void> {
        requireNetwork();

        if (this.#networkWorker !== undefined) {
            return;
        }

        const driver = new Worker(this.#networkWorkerUrl, { type: "module" });
        this.#networkWorker = driver;

        const configured = new Promise<void>((resolve) => {
            driver.addEventListener("message", () => resolve(), { once: true });
        });
        driver.postMessage({
            kind: "configure",
            options: { requestTimeoutMilliseconds: options.requestTimeoutMilliseconds },
        });
        await configured;

        const channel = new MessageChannel();
        driver.postMessage({ kind: "attach", port: channel.port2 }, [channel.port2]);

        await this.call({
            kind: "enable-network",
            port: channel.port1,
            allowedPorts: options.allowedPorts,
        }, [channel.port1]);
    }

    call<T>(call: WorkerCall, transfer: Transferable[] = []): Promise<T> {
        const id = this.#nextId++;
        return new Promise<T>((resolve, reject) => {
            this.#pending.set(id, { resolve: resolve as (value: unknown) => void, reject });
            this.#worker.postMessage({ id, call }, transfer);
        });
    }

    terminate(): void {
        this.#failAll(new Error("vpod: the runtime was terminated"));
        this.#worker.terminate();
    }
}
