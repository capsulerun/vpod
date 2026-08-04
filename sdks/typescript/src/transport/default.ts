import type { ExecutorTransport } from "./types.js";

type TransportFactory = () => Promise<ExecutorTransport>;

let factory: TransportFactory | null = null;

export function setDefaultTransportFactory(next: TransportFactory): void {
    factory = next;
}

export async function createDefaultTransport(): Promise<ExecutorTransport | undefined> {
    return factory === null ? undefined : factory();
}
