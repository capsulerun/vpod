import type { WorkerCall } from "../worker/protocol.js";
import type { NetworkBackendName } from "../net/capabilities.js";

export interface ExecutorTransport {
    ready(): Promise<number>;
    call<T>(call: WorkerCall, transfer?: Transferable[]): Promise<T>;
    terminate(): void;

    // to enable network
    readonly networkBackend?: NetworkBackendName;
}
