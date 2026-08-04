/**
 * What the guest's network can do, per backend.
 */

import { FORBIDDEN_REQUEST_HEADERS } from "./http-codec.js";

export type NetworkBackendName = "none" | "fetch" | "sockets";

export interface NetworkCapabilities {
    backend: NetworkBackendName;
    rawTcp: boolean;
    arbitraryPorts: boolean;
    corsRestricted: boolean;
    byteFaithfulHeaders: boolean;
    udp: boolean;
    strippedRequestHeaders: string[];
}

const NONE: NetworkCapabilities = {
    backend: "none",
    rawTcp: false,
    arbitraryPorts: false,
    corsRestricted: false,
    byteFaithfulHeaders: false,
    udp: false,
    strippedRequestHeaders: [],
};

const FETCH: NetworkCapabilities = {
    backend: "fetch",
    rawTcp: false,
    arbitraryPorts: false,
    corsRestricted: true,
    byteFaithfulHeaders: false,
    udp: false,
    strippedRequestHeaders: [...FORBIDDEN_REQUEST_HEADERS].sort(),
};

const SOCKETS: NetworkCapabilities = {
    backend: "sockets",
    rawTcp: true,
    arbitraryPorts: true,
    corsRestricted: false,
    byteFaithfulHeaders: true,
    udp: true,
    strippedRequestHeaders: [],
};

export function capabilitiesOf(backend: NetworkBackendName): NetworkCapabilities {
    switch (backend) {
        case "fetch":
            return { ...FETCH, strippedRequestHeaders: [...FETCH.strippedRequestHeaders] };
        case "sockets":
            return { ...SOCKETS, strippedRequestHeaders: [] };
        case "none":
            return { ...NONE, strippedRequestHeaders: [] };
    }
}

export function explainUnreachable(
    capabilities: NetworkCapabilities,
    port: number,
): string | null {
    if (capabilities.backend === "none") {
        return "this sandbox has no network. Create it with { network: true }.";
    }

    if (!capabilities.arbitraryPorts && port !== 80 && port !== 443) {
        return `port ${port} is unreachable on the ${capabilities.backend} backend, which serves only 80 and 443.`;
    }

    return null;
}
