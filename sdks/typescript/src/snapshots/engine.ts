/**
 * A snapshot's own engine.
 *
 * A catalogue entry may list `engines`, each a whole engine component built from
 * a trace of that snapshot. Any engine whose interface fingerprint matches the
 * bundled engine's can replace it, whichever release built it, because jco's
 * glue and adapter modules depend only on that interface.
 *
 * A sandbox never changes engine after it starts. When a usable engine is not
 * cached yet, the sandbox starts on the bundled engine and the image engine is
 * downloaded in the background, so the next sandbox starts on it.
 *
 * Files in the snapshot store, per engine:
 *   engine-{sha256}.download   the bytes as served (gzip)
 *   engine-{sha256}.sha256     marks the download as verified
 *   engine-{sha256}.snapshot   which snapshot it belongs to, for replacement
 */

import { authHeaders } from "./auth.js";
import { fetchCatalogue, SnapshotAuthError } from "./catalogue.js";
import { sha256Hex } from "./digest.js";
import type { SnapshotStorage } from "./store.js";
import type { EngineEntry, SnapshotEntry } from "./types.js";

const GZIP_MAGIC = [0x1f, 0x8b];
const WASM_MAGIC = [0x00, 0x61, 0x73, 0x6d];
const COMPONENT_PREAMBLE_LENGTH = 8;
const CORE_MODULE_SECTION = 1;

const downloadName = (sha256: string): string => `engine-${sha256}.download`;
const digestName = (sha256: string): string => `engine-${sha256}.sha256`;
const snapshotName = (sha256: string): string => `engine-${sha256}.snapshot`;

function versionKey(version: string): number[] {
    const match = /^(\d+)\.(\d+)\.(\d+)(.*)$/.exec(version);
    if (match === null) {
        return [-1];
    }
    const [, major, minor, patch, suffix] = match;
    const suffixNumbers = [...suffix.matchAll(/\d+/g)].map((found) => Number(found[0]));
    return [Number(major), Number(minor), Number(patch), suffix === "" ? 1 : 0, ...suffixNumbers];
}

function compareVersions(left: string, right: string): number {
    const a = versionKey(left);
    const b = versionKey(right);
    for (let index = 0; index < Math.max(a.length, b.length); index++) {
        const difference = (a[index] ?? -1) - (b[index] ?? -1);
        if (difference !== 0) {
            return difference;
        }
    }
    return 0;
}

/** The newest engine listed for this snapshot that this SDK can run. */
export function selectEngine(
    entry: SnapshotEntry,
    engineInterface: string | null,
): EngineEntry | null {
    if (engineInterface === null) {
        return null;
    }

    const usable = (entry.engines ?? []).filter(
        (engine) => engine.interface === engineInterface && engine.sha256 && engine.url,
    );
    if (usable.length === 0) {
        return null;
    }
    return usable.reduce((newest, engine) =>
        compareVersions(engine.vpod_version, newest.vpod_version) > 0 ? engine : newest,
    );
}

const startsWith = (bytes: Uint8Array, magic: number[]): boolean =>
    magic.every((byte, index) => bytes[index] === byte);

async function gunzip(bytes: Uint8Array): Promise<Uint8Array> {
    const stream = new Blob([bytes as BlobPart]).stream().pipeThrough(new DecompressionStream("gzip"));
    return new Uint8Array(await new Response(stream).arrayBuffer());
}

/** An engine download as a component: engines are served gzip, raw is accepted too. */
export async function decompressEngine(download: Uint8Array): Promise<Uint8Array> {
    const component = startsWith(download, GZIP_MAGIC) ? await gunzip(download) : download;
    if (!startsWith(component, WASM_MAGIC)) {
        throw new Error("vpod: the snapshot's engine is not a wasm component");
    }
    return component;
}

function readUnsigned(bytes: Uint8Array, offset: number): [number, number] {
    let result = 0;
    let shift = 0;
    for (;;) {
        const byte = bytes[offset++];
        if (byte === undefined) {
            throw new Error("vpod: the snapshot's engine ends in the middle of a section");
        }
        result += (byte & 0x7f) * 2 ** shift;
        if (byte < 0x80) {
            return [result, offset];
        }
        shift += 7;
    }
}

/**
 * The component's embedded core modules, named the way jco names them.
 *
 * jco's `vpod.core.wasm` is the first embedded core module byte for byte, and
 * `vpod.coreN.wasm` is module N-1, so an image engine needs no transpile.
 */
export function coreModulesOf(component: Uint8Array): Record<string, Uint8Array> {
    const modules: Uint8Array[] = [];
    let offset = COMPONENT_PREAMBLE_LENGTH;

    while (offset < component.byteLength) {
        const sectionId = component[offset];
        const [size, bodyStart] = readUnsigned(component, offset + 1);
        const bodyEnd = bodyStart + size;
        if (bodyEnd > component.byteLength) {
            throw new Error("vpod: the snapshot's engine ends in the middle of a section");
        }
        if (sectionId === CORE_MODULE_SECTION) {
            modules.push(component.subarray(bodyStart, bodyEnd));
        }
        offset = bodyEnd;
    }

    if (modules.length === 0) {
        throw new Error("vpod: the snapshot's engine embeds no core module");
    }

    return Object.fromEntries(
        modules.map((module, index) => [
            index === 0 ? "vpod.core.wasm" : `vpod.core${index + 1}.wasm`,
            module,
        ]),
    );
}

/** The engine's verified download, or null when it is not cached. */
export async function readCachedEngine(
    store: SnapshotStorage,
    engine: EngineEntry,
): Promise<Uint8Array | null> {
    if ((await store.readText(digestName(engine.sha256)))?.trim() !== engine.sha256) {
        return null;
    }

    const download = await store.read(downloadName(engine.sha256));
    if (download === null || (await sha256Hex(download)) !== engine.sha256) {
        await removeEngine(store, engine.sha256);
        return null;
    }
    return download;
}

async function removeEngine(store: SnapshotStorage, sha256: string): Promise<void> {
    await store.remove(digestName(sha256));
    await store.remove(downloadName(sha256));
    await store.remove(snapshotName(sha256));
}

async function fetchVerified(
    engine: EngineEntry,
    registryUrl: string,
    apiKey: string | undefined,
): Promise<Uint8Array> {
    const url = new URL(engine.url, registryUrl).href;
    const response = await fetch(url, { headers: authHeaders(url, registryUrl, apiKey) });

    if (response.status === 401 || response.status === 403) {
        throw new SnapshotAuthError(`vpod: engine ${engine.sha256.slice(0, 12)} returned ${response.status}`);
    }
    if (!response.ok) {
        throw new Error(`vpod: engine ${engine.sha256.slice(0, 12)} returned ${response.status} from ${url}`);
    }

    const bytes = new Uint8Array(await response.arrayBuffer());
    const actual = await sha256Hex(bytes);
    if (actual !== engine.sha256) {
        throw new Error(`vpod: checksum mismatch for engine ${engine.sha256}, got ${actual}`);
    }
    return bytes;
}

export interface EngineSource {
    registryUrl: string;
    apiKey?: string;
}

/** Download, verify and cache an engine, then drop the engine it replaces for that snapshot. */
export async function downloadEngine(
    store: SnapshotStorage,
    snapshotId: string,
    engine: EngineEntry,
    source: EngineSource,
): Promise<void> {
    let bytes: Uint8Array;
    try {
        bytes = await fetchVerified(engine, source.registryUrl, source.apiKey);
    } catch (thrown: unknown) {
        if (!(thrown instanceof SnapshotAuthError)) {
            throw thrown;
        }
        const refreshed = await fetchCatalogue(store, { ...source, force: true });
        const relisted = refreshed.snapshots
            .find((snapshot) => snapshot.id === snapshotId)
            ?.engines?.find((listed) => listed.sha256 === engine.sha256);
        if (relisted === undefined) {
            throw thrown;
        }
        bytes = await fetchVerified(relisted, source.registryUrl, source.apiKey);
    }

    await store.write(downloadName(engine.sha256), bytes);
    await store.writeText(snapshotName(engine.sha256), snapshotId);
    await store.writeText(digestName(engine.sha256), engine.sha256);

    for (const file of await store.list()) {
        const replaced = /^engine-([0-9a-f]+)\.snapshot$/.exec(file.name)?.[1];
        if (replaced === undefined || replaced === engine.sha256) {
            continue;
        }
        if ((await store.readText(file.name))?.trim() === snapshotId) {
            await removeEngine(store, replaced);
        }
    }
}

const downloadsInFlight = new Map<string, Promise<void>>();

/** Start caching an engine for the next sandbox, once per engine per process. */
export function downloadEngineInBackground(
    store: SnapshotStorage,
    snapshotId: string,
    engine: EngineEntry,
    source: EngineSource,
): Promise<void> {
    let inFlight = downloadsInFlight.get(engine.sha256);
    if (inFlight === undefined) {
        inFlight = downloadEngine(store, snapshotId, engine, source).catch((thrown: unknown) => {
            downloadsInFlight.delete(engine.sha256);
            console.warn(`vpod: could not download ${snapshotId}'s engine, staying on the bundled one. ${String(thrown)}`);
        });
        downloadsInFlight.set(engine.sha256, inFlight);
    }
    return inFlight;
}

const announced = new Set<string>();

/** Say once per process when a snapshot's engine is older than this SDK, or unusable by it. */
export function announceEngine(
    entry: SnapshotEntry,
    chosen: EngineEntry | null,
    sdkVersion: string,
): void {
    if ((entry.engines ?? []).length === 0) {
        return;
    }

    let key: string;
    let message: string;
    if (chosen === null) {
        key = `unusable:${entry.id}`;
        message =
            `vpod: ${entry.id} has an engine, but none of its builds fits vpod ${sdkVersion}, ` +
            `so it runs on the bundled engine. Rebuild the snapshot's engine to get its speed back.`;
    } else if (sdkVersion !== "0.0.0" && compareVersions(chosen.vpod_version, sdkVersion) < 0) {
        key = `older:${chosen.sha256}`;
        message =
            `vpod: ${entry.id}'s engine was built with vpod ${chosen.vpod_version}. ` +
            `It still works; rebuild it to get the fixes in ${sdkVersion}.`;
    } else {
        return;
    }

    if (!announced.has(key)) {
        announced.add(key);
        console.warn(message);
    }
}
