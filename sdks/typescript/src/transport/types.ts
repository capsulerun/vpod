import type { WorkerCall } from "../worker/protocol.js";

export interface ExecutorTransport {
    // Resolves with how long the component took to compile and instantiate.
    ready(): Promise<number>;
    call<T>(call: WorkerCall, transfer?: Transferable[]): Promise<T>;
    terminate(): void;
}
