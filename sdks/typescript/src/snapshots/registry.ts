export const DEFAULT_REGISTRY_URL = "https://registry.vpod.sh/v1/snapshots.json";

function fromEnvironment(name: string): string | undefined {
    const environment = (
        globalThis as { process?: { env?: Record<string, string | undefined> } }
    ).process?.env;
    const configured = environment?.[name];
    return configured === undefined || configured === "" ? undefined : configured;
}

export function resolveRegistryUrl(explicit: string | undefined): string {
    if (explicit !== undefined && explicit !== "") {
        return explicit;
    }
    return fromEnvironment("VPOD_REGISTRY") ?? DEFAULT_REGISTRY_URL;
}

export function registryCacheKey(registryUrl: string): string {
    let hash = 0x811c9dc5;
    for (let i = 0; i < registryUrl.length; i += 1) {
        hash ^= registryUrl.charCodeAt(i);
        hash = Math.imul(hash, 0x01000193) >>> 0;
    }
    return hash.toString(16).padStart(8, "0");
}
