import { fetchCatalogue, resolveSnapshot } from "./catalogue.js";
import { resolveRegistryUrl } from "./registry.js";
import { SnapshotStore } from "./store.js";
import type { CatalogueOptions } from "./catalogue.js";
import type { SnapshotStorage } from "./store.js";
import type { SnapshotEntry } from "./types.js";

export { DEFAULT_REGISTRY_URL, fetchCatalogue, resolveSnapshot } from "./catalogue.js";
export { resolveRegistryUrl } from "./registry.js";
export { pullSnapshot, evictById } from "./pull.js";
export { SnapshotStore } from "./store.js";
export type { CachedFile, SnapshotStorage } from "./store.js";
export type { Catalogue, SnapshotEntry, PulledSnapshot, SnapshotSource } from "./types.js";
export type { CatalogueOptions } from "./catalogue.js";
export type { PullOptions } from "./pull.js";

export interface CacheOptions {
    store?: SnapshotStorage | null;
}

export interface ClearOptions extends CacheOptions {
    instances?: boolean;
}

export interface CachedSnapshot {
    id: string;
    byteLength: number;
}

async function defaultStore(explicit: SnapshotStorage | null | undefined) {
    if (explicit !== undefined) {
        return explicit;
    }
    return SnapshotStore.available() ? await SnapshotStore.open() : null;
}

export async function cached(options: CacheOptions = {}): Promise<CachedSnapshot[]> {
    const store = await defaultStore(options.store);
    if (store === null) {
        return [];
    }

    return (await store.list())
        .filter((file) => file.name.endsWith(".snap"))
        .map((file) => ({
            id: file.name.slice(0, -".snap".length),
            byteLength: file.byteLength,
        }));
}

export async function clear(options: ClearOptions = {}): Promise<number> {
    const store = await defaultStore(options.store);
    if (store === null) {
        return 0;
    }

    const held = await store.list();

    const digests = new Set(
        held
            .filter((file) => file.name.endsWith(".sha256"))
            .map((file) => file.name.slice(0, -".sha256".length)),
    );

    const disposable = (name: string): boolean => {
        if (name.endsWith(".sha256")) return true;
        if (name.endsWith(".snap")) return digests.has(name.slice(0, -".snap".length));
        if (name.startsWith("catalogue")) return true;
        if (options.instances !== true) return false;
        return name.endsWith(".delta") || name === "instances.json";
    };

    let reclaimed = 0;
    for (const file of held) {
        if (!disposable(file.name)) {
            continue;
        }
        await store.remove(file.name);
        reclaimed += file.byteLength;
    }
    return reclaimed;
}

export async function catalog(options: CatalogueOptions = {}): Promise<SnapshotEntry[]> {
    const store = SnapshotStore.available() ? await SnapshotStore.open() : null;
    const catalogue = await fetchCatalogue(store, options);
    return catalogue.snapshots;
}

export async function resolve(
    name: string,
    options: CatalogueOptions = {},
): Promise<SnapshotEntry> {
    return resolveSnapshot(
        await catalog(options),
        name,
        resolveRegistryUrl(options.registryUrl),
    );
}
