import { keyFingerprint, PUBLIC_REGISTRY_URL } from "./auth.js";

export { PRIVATE_REGISTRY_URL, PUBLIC_REGISTRY_URL, resolveRegistryUrl } from "./auth.js";

export const DEFAULT_REGISTRY_URL = PUBLIC_REGISTRY_URL;

export function registryCacheKey(registryUrl: string, apiKey?: string): string {
    const material =
        apiKey === undefined ? registryUrl : `${registryUrl}#${keyFingerprint(apiKey)}`;
    let hash = 0x811c9dc5;

    for (let i = 0; i < material.length; i += 1) {
        hash ^= material.charCodeAt(i);
        hash = Math.imul(hash, 0x01000193) >>> 0;
    }
    return hash.toString(16).padStart(8, "0");
}
