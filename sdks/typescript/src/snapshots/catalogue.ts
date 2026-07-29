import type { SnapshotStore } from "./store.js";
import type { Catalogue, SnapshotEntry } from "./types.js";

export const DEFAULT_REGISTRY_URL = "https://registry.vpod.sh/v1/snapshots.json";

const CATALOGUE_FILE = "catalogue.json";
const CATALOGUE_FETCHED_AT_FILE = "catalogue.fetched-at";
const DEFAULT_TTL_SECONDS = 86_400;

export interface CatalogueOptions {
    registryUrl?: string;
    ttlSeconds?: number;
    force?: boolean;
}

export async function fetchCatalogue(
    store: SnapshotStore | null,
    options: CatalogueOptions = {},
): Promise<Catalogue> {
    const registryUrl = options.registryUrl ?? DEFAULT_REGISTRY_URL;
    const ttlSeconds = options.ttlSeconds ?? DEFAULT_TTL_SECONDS;

    if (store !== null && options.force !== true) {
        const cached = await readCachedCatalogue(store, ttlSeconds);
        if (cached !== null) {
            return cached;
        }
    }

    let response: Response;
    try {
        response = await fetch(registryUrl);
    } catch (thrown: unknown) {
        const stale = store === null ? null : await readCachedCatalogue(store, Infinity);

        if (stale !== null) {
            return stale;
        }

        throw new Error(
            `vpod: could not fetch the snapshot registry at ${registryUrl}. ` +
                `If the host sends no Access-Control-Allow-Origin, a page cannot ` +
                `read it at all. Underlying error: ${String(thrown)}`,
        );
    }

    if (!response.ok) {
        throw new Error(
            `vpod: snapshot registry at ${registryUrl} returned ${response.status}`,
        );
    }

    const catalogue = (await response.json()) as Catalogue;

    if (store !== null) {
        await store.writeText(CATALOGUE_FILE, JSON.stringify(catalogue));
        await store.writeText(CATALOGUE_FETCHED_AT_FILE, String(Date.now()));
    }

    return catalogue;
}

async function readCachedCatalogue(
    store: SnapshotStore,
    ttlSeconds: number,
): Promise<Catalogue | null> {
    const text = await store.readText(CATALOGUE_FILE);
    if (text === null) {
        return null;
    }

    if (ttlSeconds !== Infinity) {
        const fetchedAt = Number(await store.readText(CATALOGUE_FETCHED_AT_FILE));
        if (!Number.isFinite(fetchedAt)) {
            return null;
        }
        if ((Date.now() - fetchedAt) / 1000 >= ttlSeconds) {
            return null;
        }
    }

    try {
        return JSON.parse(text) as Catalogue;
    } catch {
        return null;
    }
}

/**
 * Resolve `name:tag` against the catalogue.
 */
export function resolveSnapshot(
    snapshots: SnapshotEntry[],
    name: string,
): SnapshotEntry {
    const separator = name.indexOf(":");
    const wantedName = separator === -1 ? name : name.slice(0, separator);
    const wantedTag = separator === -1 ? "latest" : name.slice(separator + 1) || "latest";

    for (const snapshot of snapshots) {
        const nameMatches = snapshot.name === wantedName;
        const tagMatches = wantedTag === "latest" || wantedTag === snapshot.tag;
        if (snapshot.id === name || (nameMatches && tagMatches)) {
            return snapshot;
        }
    }

    const available = snapshots.map((s) => `${s.name}:${s.tag}`).join(", ");
    throw new Error(`vpod: snapshot '${name}' not found. Available: ${available}`);
}
