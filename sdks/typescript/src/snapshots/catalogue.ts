import { authHeaders, checkApiKeyKind, resolveApiKey } from "./auth.js";
import { registryCacheKey, resolveRegistryUrl } from "./registry.js";
import type { SnapshotStorage } from "./store.js";
import type { Catalogue, SnapshotEntry } from "./types.js";

export { DEFAULT_REGISTRY_URL } from "./registry.js";

const DEFAULT_TTL_SECONDS = 86_400;

const catalogueFile = async (registryUrl: string, apiKey?: string): Promise<string> =>
    `catalogue-${await registryCacheKey(registryUrl, apiKey)}.json`;

const catalogueFetchedAtFile = async (
    registryUrl: string,
    apiKey?: string,
): Promise<string> => `catalogue-${await registryCacheKey(registryUrl, apiKey)}.fetched-at`;

export interface CatalogueOptions {
    registryUrl?: string;
    apiKey?: string;
    ttlSeconds?: number;
    force?: boolean;
}

export class SnapshotAuthError extends Error {}

export async function fetchCatalogue(
    store: SnapshotStorage | null,
    options: CatalogueOptions = {},
): Promise<Catalogue> {
    const apiKey = resolveApiKey(options.apiKey);
    if (apiKey !== undefined) {
        checkApiKeyKind(apiKey);
    }
    const registryUrl = resolveRegistryUrl(options.registryUrl, apiKey);
    const ttlSeconds = options.ttlSeconds ?? DEFAULT_TTL_SECONDS;

    if (store !== null && options.force !== true) {
        const cached = await readCachedCatalogue(store, registryUrl, ttlSeconds, apiKey);
        if (cached !== null) {
            return cached;
        }
    }

    let response: Response;
    try {
        response = await fetch(registryUrl, {
            headers: authHeaders(registryUrl, registryUrl, apiKey),
        });
    } catch (thrown: unknown) {
        const stale =
            store === null
                ? null
                : await readCachedCatalogue(store, registryUrl, Infinity, apiKey);

        if (stale !== null) {
            return stale;
        }

        throw new Error(
            `vpod: could not fetch the snapshot registry at ${registryUrl}. ` +
                `If the host sends no Access-Control-Allow-Origin, a page cannot ` +
                `read it at all. Underlying error: ${String(thrown)}`,
        );
    }

    if (response.status === 401 || response.status === 403) {
        throw new SnapshotAuthError(
            `vpod: ${registryUrl} refused the request (${response.status}). ` +
                (apiKey === undefined
                    ? "No API key was sent. Set VPOD_API_KEY or pass apiKey."
                    : "The key may be revoked, or it may belong to a different " +
                      "organisation than the snapshot you asked for."),
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
            await store.writeText(
                await catalogueFile(registryUrl, apiKey),
                JSON.stringify(catalogue),
            );
            await store.writeText(
                await catalogueFetchedAtFile(registryUrl, apiKey),
                String(Date.now()),
            );
        } catch {
        }
    }

    return catalogue;
}

async function readCachedCatalogue(
    store: SnapshotStorage,
    registryUrl: string,
    ttlSeconds: number,
    apiKey?: string,
): Promise<Catalogue | null> {
    const text = await store.readText(await catalogueFile(registryUrl, apiKey));
    if (text === null) {
        return null;
    }

    if (ttlSeconds !== Infinity) {
        const fetchedAt = Number(
            await store.readText(await catalogueFetchedAtFile(registryUrl, apiKey)),
        );
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
    authenticated = false,
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
    const credentials = authenticated
        ? " An API key WAS sent, so this catalogue is what that key can reach."
        : " No API key was sent, so only public snapshots were searched.";

    throw new Error(
        `vpod: snapshot '${name}' not found${searched}. Available: ${available}.${credentials}`,
    );
}
