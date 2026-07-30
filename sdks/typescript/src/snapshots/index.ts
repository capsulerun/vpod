import { fetchCatalogue, resolveSnapshot } from "./catalogue.js";
import { SnapshotStore } from "./store.js";
import type { CatalogueOptions } from "./catalogue.js";
import type { SnapshotEntry } from "./types.js";

export { DEFAULT_REGISTRY_URL, fetchCatalogue, resolveSnapshot } from "./catalogue.js";
export { pullSnapshot, evictById } from "./pull.js";
export { SnapshotStore } from "./store.js";
export type { Catalogue, SnapshotEntry, PulledSnapshot, SnapshotSource } from "./types.js";
export type { CatalogueOptions } from "./catalogue.js";
export type { PullOptions } from "./pull.js";

export async function catalog(options: CatalogueOptions = {}): Promise<SnapshotEntry[]> {
    const store = SnapshotStore.available() ? await SnapshotStore.open() : null;
    const catalogue = await fetchCatalogue(store, options);
    return catalogue.snapshots;
}

export async function resolve(
    name: string,
    options: CatalogueOptions = {},
): Promise<SnapshotEntry> {
    return resolveSnapshot(await catalog(options), name);
}
