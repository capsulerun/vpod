/**
 * `wasi:sockets/ip-name-lookup` for the Node target.
 *
 * preview2-shim's own implementation has been wrong in both directions: 0.17.9 resolves nothing at all, and 0.18.0 onward return each address wrapped in a one-element array that jco's variant lowering rejects.
 * Written to be a no-op the day upstream lands the fix, so deleting it is safe and `tests/unit/host-resolver.test.mjs` says when.
 */

import { sockets } from "@bytecodealliance/preview2-shim";

interface HostStream {
    resolveNextAddress(): unknown;
    subscribe(): object;
    [Symbol.dispose]?(): void;
}

class ResolveAddressStream {
    #host: HostStream;

    constructor(host: HostStream) {
        this.#host = host;
    }

    resolveNextAddress(): unknown {
        const next = this.#host.resolveNextAddress();
        return Array.isArray(next) ? next[0] : next;
    }

    subscribe(): object {
        return this.#host.subscribe();
    }

    [Symbol.dispose](): void {
        this.#host[Symbol.dispose]?.();
    }
}

export const ipNameLookup = {
    ResolveAddressStream,
    resolveAddresses: (network: unknown, name: string): ResolveAddressStream =>
        new ResolveAddressStream(
            (sockets.ipNameLookup as unknown as {
                resolveAddresses(network: unknown, name: string): HostStream;
            }).resolveAddresses(network, name),
        ),
};
