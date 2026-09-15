import type { ExecutorTransport } from "./types.js";

export interface DefaultTransportOptions {
    /** Core modules replacing the bundled ones by name, as a snapshot's own engine does. */
    coreModules?: Record<string, Uint8Array>;
}

type TransportFactory = (options: DefaultTransportOptions) => Promise<ExecutorTransport>;

let factory: TransportFactory | null = null;

export function setDefaultTransportFactory(next: TransportFactory): void {
    factory = next;
}

export async function createDefaultTransport(
    options: DefaultTransportOptions = {},
): Promise<ExecutorTransport | undefined> {
    return factory === null ? undefined : factory(options);
}
