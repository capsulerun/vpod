import { fetchCatalogue, resolveSnapshot, type CatalogueOptions } from "./catalogue.js";
import { sha256Hex } from "./digest.js";
import { SnapshotStore } from "./store.js";
import type { PulledSnapshot, SnapshotEntry } from "./types.js";

export interface PullOptions extends CatalogueOptions {
    name?: string;
    onProgress?(loaded: number, total: number): void;
}

export async function pullSnapshot(options: PullOptions = {}): Promise<PulledSnapshot> {
    const name = options.name ?? "vsnap-base:latest";
    const store = SnapshotStore.available() ? await SnapshotStore.open() : null;

    const catalogue = await fetchCatalogue(store, options);
    const entry = resolveSnapshot(catalogue.snapshots, name);

    if (store !== null) {
        const cached = await readVerifiedFromStore(store, entry);
        if (cached !== null) {
            return cached;
        }
    }

    return downloadAndStore(store, entry, options.onProgress);
}

const snapshotFileName = (entry: SnapshotEntry): string => `${entry.id}.snap`;
const digestFileName = (entry: SnapshotEntry): string => `${entry.id}.sha256`;

async function readVerifiedFromStore(
    store: SnapshotStore,
    entry: SnapshotEntry,
): Promise<PulledSnapshot | null> {
    const recordedDigest = await store.readText(digestFileName(entry));
    if (recordedDigest?.trim() !== entry.sha256) {
        return null;
    }

    const startedAt = performance.now();
    const bytes = await store.read(snapshotFileName(entry));
    if (bytes === null) {
        return null;
    }
    const fetchMilliseconds = performance.now() - startedAt;

    const verifyStartedAt = performance.now();
    const actualDigest = await sha256Hex(bytes);
    const verifyMilliseconds = performance.now() - verifyStartedAt;

    if (actualDigest !== entry.sha256) {
        await evict(store, entry);
        return null;
    }

    return {
        entry,
        bytes,
        source: "opfs",
        fetchMilliseconds,
        verifyMilliseconds,
        storeMilliseconds: 0,
    };
}

async function downloadAndStore(
    store: SnapshotStore | null,
    entry: SnapshotEntry,
    onProgress?: (loaded: number, total: number) => void,
): Promise<PulledSnapshot> {
    const startedAt = performance.now();
    const response = await fetch(entry.url);
    if (!response.ok) {
        throw new Error(
            `vpod: snapshot ${entry.id} returned ${response.status} from ${entry.url}`,
        );
    }

    const bytes = await readBody(response, entry.size, onProgress);
    const fetchMilliseconds = performance.now() - startedAt;

    const verifyStartedAt = performance.now();
    const actualDigest = await sha256Hex(bytes);
    const verifyMilliseconds = performance.now() - verifyStartedAt;

    if (actualDigest !== entry.sha256) {
        throw new Error(
            `vpod: checksum mismatch for ${entry.id}. Expected ${entry.sha256}, got ${actualDigest}`,
        );
    }

    let storeMilliseconds = 0;
    if (store !== null) {
        const storeStartedAt = performance.now();
        try {
            await store.write(snapshotFileName(entry), bytes);
            await store.writeText(digestFileName(entry), entry.sha256);
        } catch (thrown: unknown) {
            await evict(store, entry);
            console.warn(
                `vpod: could not cache ${entry.id} in OPFS, continuing uncached. ${String(thrown)}`,
            );
        }
        storeMilliseconds = performance.now() - storeStartedAt;
    }

    return {
        entry,
        bytes,
        source: "network",
        fetchMilliseconds,
        verifyMilliseconds,
        storeMilliseconds,
    };
}

async function readBody(
    response: Response,
    expectedSize: number,
    onProgress?: (loaded: number, total: number) => void,
): Promise<Uint8Array> {
    if (response.body === null || onProgress === undefined) {
        return new Uint8Array(await response.arrayBuffer());
    }

    const total = Number(response.headers.get("content-length") ?? expectedSize);
    const reader = response.body.getReader();
    const chunks: Uint8Array[] = [];
    let loaded = 0;

    for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        chunks.push(value);
        loaded += value.byteLength;
        onProgress(loaded, total);
    }

    const bytes = new Uint8Array(loaded);
    let offset = 0;
    for (const chunk of chunks) {
        bytes.set(chunk, offset);
        offset += chunk.byteLength;
    }
    return bytes;
}

export async function evict(store: SnapshotStore, entry: SnapshotEntry): Promise<void> {
    await store.remove(snapshotFileName(entry));
    await store.remove(digestFileName(entry));
}

export async function evictById(id: string): Promise<void> {
    if (!SnapshotStore.available()) return;
    const store = await SnapshotStore.open();
    await store.remove(`${id}.snap`);
    await store.remove(`${id}.sha256`);
}
