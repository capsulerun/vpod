/**
 * the component transpiled against the stock Node shims, which wrap `node:net` and give the guest real TCP.
 */

import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { pullSnapshot } from "../snapshots/pull.js";
import { componentImports, loadCoreModule } from "./component-imports.js";
import { FileSnapshotStore } from "./store.js";
import type { ComponentModule } from "../worker/component-imports.js";
import type { ExecutorTransport } from "../transport/types.js";
import type { ExecutionResult, WorkerCall } from "../worker/protocol.js";

interface Executor {
    sessionStart(snapshotPath: string, command: string, prompt: string, mounts: never[]): bigint;
    sessionExec(handle: bigint, code: string, timeout: bigint | undefined): ExecutionResult;
    sessionExecSlice(
        handle: bigint,
        code: string | undefined,
        timeout: bigint | undefined,
        sliceNanos: bigint,
    ): ExecutionResult | undefined;
    sessionInterrupt(handle: bigint): void;
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

export interface NodeTransportOptions {
    componentUrl?: string | URL;
    cacheDirectory?: string;
}

export class NodeDispatcher {
    readonly #executor: Executor;
    readonly #store: FileSnapshotStore;
    readonly #loadMilliseconds: number;

    #scratch: string | null = null;
    #deltaCount = 0;

    constructor(executor: Executor, store: FileSnapshotStore, loadMilliseconds: number) {
        this.#executor = executor;
        this.#store = store;
        this.#loadMilliseconds = loadMilliseconds;
    }

    get loadMilliseconds(): number {
        return this.#loadMilliseconds;
    }

    async #scratchDirectory(): Promise<string> {
        this.#scratch ??= await mkdtemp(join(tmpdir(), "vpod-"));
        return this.#scratch;
    }

    async #writeScratch(name: string, bytes: Uint8Array): Promise<string> {
        const path = join(await this.#scratchDirectory(), name);
        await writeFile(path, bytes);
        return path;
    }

    async handle(call: WorkerCall): Promise<unknown> {
        switch (call.kind) {
            case "pull-snapshot": {
                const pulled = await pullSnapshot({
                    name: call.name,
                    registryUrl: call.registryUrl,
                    store: await this.#store.ready(),
                });

                return {
                    snapshotPath: this.#store.pathFor(`${pulled.entry.id}.snap`),
                    id: pulled.entry.id,
                    byteLength: pulled.bytes.byteLength,
                    source: pulled.source,
                    fetchMilliseconds: pulled.fetchMilliseconds,
                    verifyMilliseconds: pulled.verifyMilliseconds,
                    storeMilliseconds: pulled.storeMilliseconds,
                };
            }

            case "storage-quota":
                return null;

            case "fetch-snapshot": {
                const response = await fetch(call.url);
                if (!response.ok) {
                    throw new Error(
                        `vpod: snapshot fetch failed with ${response.status} for ${call.url}`,
                    );
                }
                const bytes = new Uint8Array(await response.arrayBuffer());
                const name = call.name ?? call.url.split("/").pop()!;
                return {
                    snapshotPath: await this.#writeScratch(name, bytes),
                    byteLength: bytes.byteLength,
                };
            }

            case "mount-snapshot": {
                const bytes = new Uint8Array(call.bytes);
                return {
                    snapshotPath: await this.#writeScratch(call.name, bytes),
                    byteLength: bytes.byteLength,
                };
            }

            case "session-start":
                return this.#executor.sessionStart(
                    call.snapshotPath,
                    call.command,
                    call.prompt,
                    [],
                );

            case "session-exec":
                return this.#executor.sessionExec(
                    call.handle,
                    call.code,
                    call.timeoutSeconds ?? undefined,
                );

            case "session-exec-slice":
                return (
                    this.#executor.sessionExecSlice(
                        call.handle,
                        call.code ?? undefined,
                        call.timeoutSeconds ?? undefined,
                        call.sliceNanos,
                    ) ?? null
                );

            case "session-interrupt":
                this.#executor.sessionInterrupt(call.handle);
                return null;

            case "session-close":
                this.#executor.sessionClose(call.handle);
                return null;

            case "session-suspend": {
                const path = join(
                    await this.#scratchDirectory(),
                    `suspend-${this.#deltaCount++}.bin`,
                );
                this.#executor.sessionSuspend(call.handle, path);

                const bytes = new Uint8Array(readFileSync(path));
                await rm(path, { force: true });
                const copy = bytes.slice();
                return { deltaBytes: copy.buffer, byteLength: copy.byteLength };
            }

            case "session-resume": {
                const path = await this.#writeScratch(
                    `resume-${this.#deltaCount++}.bin`,
                    new Uint8Array(call.deltaBytes),
                );
                try {
                    return this.#executor.sessionResume(
                        call.snapshotPath,
                        path,
                        call.command,
                        call.prompt,
                        [],
                    );
                } finally {
                    await rm(path, { force: true });
                }
            }

            case "poll-stats":
                return { spinCount: 0, spinNanoseconds: 0 };

            case "component-load-milliseconds":
                return this.#loadMilliseconds;

            case "enable-network":
                throw new Error(
                    "vpod: Node already has a real network through node:net. " +
                        "The fetch transport is for browsers, which cannot open sockets.",
                );
        }
    }

    dispose(): void {
        if (this.#scratch !== null) {
            void rm(this.#scratch, { recursive: true, force: true });
            this.#scratch = null;
        }
    }
}

export async function loadNodeDispatcher(
    options: NodeTransportOptions = {},
): Promise<NodeDispatcher> {
    const componentUrl =
        options.componentUrl ?? new URL("../component-node/vpod.js", import.meta.url);

    const startedAt = performance.now();
    const module = (await import(String(componentUrl))) as ComponentModule<{
        executor: Executor;
    }>;
    const { executor } = await module.instantiate(
        (name) => loadCoreModule(name, new URL(String(componentUrl))),
        componentImports,
    );
    const loadMilliseconds = performance.now() - startedAt;

    return new NodeDispatcher(
        executor,
        new FileSnapshotStore(options.cacheDirectory),
        loadMilliseconds,
    );
}

class InlineNodeTransport implements ExecutorTransport {
    readonly networkBackend = "sockets" as const;

    readonly #dispatcher: NodeDispatcher;

    constructor(dispatcher: NodeDispatcher) {
        this.#dispatcher = dispatcher;
    }

    ready(): Promise<number> {
        return Promise.resolve(this.#dispatcher.loadMilliseconds);
    }

    async call<T>(call: WorkerCall): Promise<T> {
        return (await this.#dispatcher.handle(call)) as T;
    }

    terminate(): void {
        this.#dispatcher.dispose();
    }
}

export async function createNodeTransport(
    options: NodeTransportOptions = {},
): Promise<ExecutorTransport> {
    return new InlineNodeTransport(await loadNodeDispatcher(options));
}
