/**
 * Whether this page can run a sandbox with networking.
 */

export interface NetworkAvailability {
    available: boolean;
    reason: string | undefined;
}

const HEADER_ADVICE =
    "Serve the page with 'Cross-Origin-Opener-Policy: same-origin' and " +
    "'Cross-Origin-Embedder-Policy: require-corp'. Offline sandboxes work without them.";

export function networkAvailability(): NetworkAvailability {
    if (typeof SharedArrayBuffer === "undefined") {
        return {
            available: false,
            reason: `SharedArrayBuffer is unavailable, so the emulator cannot receive bytes while a guest runs. ${HEADER_ADVICE}`,
        };
    }

    const isolated = (globalThis as { crossOriginIsolated?: boolean }).crossOriginIsolated;
    if (isolated === false) {
        return {
            available: false,
            reason: `This page is not cross-origin isolated. ${HEADER_ADVICE}`,
        };
    }

    if (typeof Worker === "undefined") {
        return {
            available: false,
            reason: "Workers are unavailable, so fetch cannot run off the emulator's thread.",
        };
    }

    return { available: true, reason: undefined };
}

export function requireNetwork(): void {
    const availability = networkAvailability();
    if (!availability.available) {
        throw new Error(`vpod: networking is unavailable. ${availability.reason}`);
    }
}
