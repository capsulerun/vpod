import {
    deltaPath,
    mountDelta,
    mountSnapshot,
    readGuestFile,
    removeGuestFile,
} from "../shims/filesystem.js";
import { pollStats } from "../shims/io.js";
import { FetchSocketBackend } from "../net/fetch-backend.js";
import { setSocketBackend, socketBackendName } from "../shims/sockets.js";
import { evictById, pullSnapshot } from "../snapshots/pull.js";
import { SnapshotStore } from "../snapshots/store.js";
import { componentImports } from "./component-imports.js";
import type { ComponentModule, CoreModuleLoader } from "./component-imports.js";
import type { DriverCommand } from "../net/driver-protocol.js";
import type { ExecutionResult, WorkerCall } from "./protocol.js";

async function announceHostTerminatedTls(): Promise<void> {
    const cli = (await import("../shims/cli.js")) as unknown as {
        _setEnv(env: Record<string, string>): void;
    };
    cli._setEnv({ VPOD_HOST_TLS: "1" });
}

export interface Executor {
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
    sessionExecSlice(
        handle: bigint,
        code: string | undefined,
        timeout: bigint | undefined,
        sliceNanos: bigint,
        mode: string,
    ): ExecutionResult | undefined;
    sessionInterrupt(handle: bigint): void;
    sessionStdin(handle: bigint, data: Uint8Array): void;
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

const sharedComponents = new Map<string, Promise<{ executor: Executor }>>();

export class Dispatcher {
    #executor: Executor | null = null;
    #componentLoadMilliseconds = 0;
    #mountedSnapshotIds = new Map<string, string>();
    #nextDeltaId = 1;

    async load(componentUrl: string, getCoreModule?: CoreModuleLoader): Promise<number> {
        const shareKey = getCoreModule === undefined ? componentUrl : null;

        return this.#instantiate(
            async () =>
                (await import(
                    /* @vite-ignore */ /* webpackIgnore: true */ componentUrl
                )) as ComponentModule<{ executor: Executor }>,
            getCoreModule,
            shareKey,
        );
    }

    async loadModule(
        module: ComponentModule<{ executor: Executor }>,
        getCoreModule?: CoreModuleLoader,
    ): Promise<number> {
        return this.#instantiate(async () => module, getCoreModule, null);
    }

    async #instantiate(
        open: () => Promise<ComponentModule<{ executor: Executor }>>,
        getCoreModule: CoreModuleLoader | undefined,
        shareKey: string | null,
    ): Promise<number> {
        const startedAt = performance.now();
        let component = shareKey === null ? undefined : sharedComponents.get(shareKey);

        if (component === undefined) {
            component = open().then((module) =>
                module.instantiate(getCoreModule, componentImports),
            );

            if (shareKey !== null) {
                sharedComponents.set(shareKey, component);
                component.catch(() => sharedComponents.delete(shareKey));
            }
        }

        this.#executor = (await component).executor;
        this.#componentLoadMilliseconds = performance.now() - startedAt;
        return this.#componentLoadMilliseconds;
    }

    get componentLoadMilliseconds(): number {
        return this.#componentLoadMilliseconds;
    }

    #requireExecutor(): Executor {
        if (this.#executor === null) {
            throw new Error("vpod: the component is not loaded yet");
        }
        return this.#executor;
    }

    async #explainRejectedSnapshot(
        snapshotPath: string,
        thrown: unknown,
    ): Promise<unknown> {
        const message = String((thrown as { payload?: unknown })?.payload ?? thrown);
        const id = this.#mountedSnapshotIds.get(snapshotPath);
        if (!message.includes("invalid snapshot magic") || id === undefined) {
            return thrown;
        }

        await evictById(id);
        return new Error(
            `vpod: the guest rejected snapshot '${id}' with "invalid snapshot magic". ` +
                `A snapshot compressed twice looks exactly like this. The cached copy ` +
                `has been dropped so a retry refetches; if the registry itself serves ` +
                `it double-framed, the retry fails identically.`,
        );
    }

    async handle(call: WorkerCall): Promise<unknown> {
        switch (call.kind) {
            case "pull-snapshot": {
                const pulled = await pullSnapshot({
                    name: call.name,
                    registryUrl: call.registryUrl,
                    apiKey: call.apiKey,
                    force: call.force,
                });
                const snapshotPath = mountSnapshot(`${pulled.entry.id}.snap`, pulled.bytes);
                this.#mountedSnapshotIds.set(snapshotPath, pulled.entry.id);
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
                        `vpod: snapshot fetch failed with ${response.status} for ${call.url}`,
                    );
                }
                const name = call.name ?? call.url.split("/").pop()!;
                const bytes = new Uint8Array(await response.arrayBuffer());
                return { snapshotPath: mountSnapshot(name, bytes), byteLength: bytes.byteLength };
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
                    return this.#requireExecutor().sessionStart(
                        call.snapshotPath,
                        call.command,
                        call.prompt,
                        [],
                    );
                } catch (thrown: unknown) {
                    throw await this.#explainRejectedSnapshot(call.snapshotPath, thrown);
                }

            case "session-exec":
                return this.#requireExecutor().sessionExec(
                    call.handle,
                    call.code,
                    call.timeoutSeconds ?? undefined,
                );

            case "session-exec-slice":
                return this.#requireExecutor().sessionExecSlice(
                    call.handle,
                    call.code ?? undefined,
                    call.timeoutSeconds ?? undefined,
                    call.sliceNanos,
                    call.mode,
                );

            case "session-interrupt":
                this.#requireExecutor().sessionInterrupt(call.handle);
                return null;

            case "session-stdin":
                this.#requireExecutor().sessionStdin(call.handle, call.data);
                return null;

            case "session-close":
                this.#requireExecutor().sessionClose(call.handle);
                return null;

            case "session-suspend": {
                const name = `suspend-${this.#nextDeltaId++}.bin`;
                const path = deltaPath(name);
                this.#requireExecutor().sessionSuspend(call.handle, path);

                const bytes = readGuestFile(path);
                if (bytes === null) {
                    throw new Error(`vpod: suspend produced no delta at ${path}`);
                }
                removeGuestFile(path);

                const copy = bytes.slice();
                return { deltaBytes: copy.buffer, byteLength: copy.byteLength };
            }

            case "session-resume": {
                const name = `resume-${this.#nextDeltaId++}.bin`;
                const path = mountDelta(name, new Uint8Array(call.deltaBytes));
                try {
                    return this.#requireExecutor().sessionResume(
                        call.snapshotPath,
                        path,
                        call.command,
                        call.prompt,
                        [],
                    );
                } finally {
                    removeGuestFile(path);
                }
            }

            case "enable-network": {
                await announceHostTerminatedTls();

                const port = call.port;
                setSocketBackend(
                    new FetchSocketBackend(
                        (command: DriverCommand, transfer: Transferable[] = []) => {
                            port.postMessage(command, transfer);
                        },
                        { allowedPorts: call.allowedPorts },
                    ),
                );

                return { backend: socketBackendName() };
            }

            case "poll-stats":
                return pollStats();

            case "component-load-milliseconds":
                return this.#componentLoadMilliseconds;
        }
    }
}
