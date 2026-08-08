import { registryCacheKey, resolveRegistryUrl } from "./registry.js";
import type { SnapshotStorage } from "./store.js";
import type { Catalogue, SnapshotEntry } from "./types.js";

export { DEFAULT_REGISTRY_URL } from "./registry.js";

const DEFAULT_TTL_SECONDS = 86_400;

const catalogueFile = (registryUrl: string): string =>
    `catalogue-${registryCacheKey(registryUrl)}.json`;

const catalogueFetchedAtFile = (registryUrl: string): string =>
    `catalogue-${registryCacheKey(registryUrl)}.fetched-at`;

export interface CatalogueOptions {
    registryUrl?: string;
    ttlSeconds?: number;
    force?: boolean;
}

export async function fetchCatalogue(
    store: SnapshotStorage | null,
    options: CatalogueOptions = {},
): Promise<Catalogue> {
    const registryUrl = resolveRegistryUrl(options.registryUrl);
    const ttlSeconds = options.ttlSeconds ?? DEFAULT_TTL_SECONDS;

    if (store !== null && options.force !== true) {
        const cached = await readCachedCatalogue(store, registryUrl, ttlSeconds);
        if (cached !== null) {
            return cached;
        }
    }

    let response: Response;
    try {
        response = await fetch(registryUrl);
    } catch (thrown: unknown) {
        const stale =
            store === null ? null : await readCachedCatalogue(store, registryUrl, Infinity);

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

        try {
            await store.writeText(catalogueFile(registryUrl), JSON.stringify(catalogue));
            await store.writeText(catalogueFetchedAtFile(registryUrl), String(Date.now()));
        } catch {
        }
    }

    return catalogue;
}

async function readCachedCatalogue(
    store: SnapshotStorage,
    registryUrl: string,
    ttlSeconds: number,
): Promise<Catalogue | null> {
    const text = await store.readText(catalogueFile(registryUrl));
    if (text === null) {
        return null;
    }

    if (ttlSeconds !== Infinity) {
        const fetchedAt = Number(await store.readText(catalogueFetchedAtFile(registryUrl)));
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

export function resolveSnapshot(
    snapshots: SnapshotEntry[],
    name: string,
    registryUrl?: string,
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

    const available = snapshots.map((s) => `${s.name}:${s.tag}`).join(", ") || "nothing";
    const searched = registryUrl === undefined ? "" : ` in ${registryUrl}`;
    throw new Error(
        `vpod: snapshot '${name}' not found${searched}. Available: ${available}`,
    );
}
