import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { gzipSync as gzipBuffer } from "node:zlib";
import { afterEach, describe, it } from "node:test";

import { distPath } from "../helpers.mjs";

const { snapshots } = await import(distPath("index.js"));
const { coreModulesOf, decompressEngine, downloadEngine, readCachedEngine, selectEngine } = snapshots;

const INTERFACE = `v1:${"a".repeat(64)}`;
const OTHER_INTERFACE = `v1:${"b".repeat(64)}`;
const REGISTRY = "https://api.vpod.sh/v1/snapshots.json";

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const gzipSync = (bytes) => new Uint8Array(gzipBuffer(bytes));

function engine(sha, version, overrides = {}) {
    return { sha256: sha, vpod_version: version, interface: INTERFACE, url: "https://blob/e", size: 1, ...overrides };
}

function unsigned(value) {
    const bytes = [];
    do {
        let byte = value & 0x7f;
        value >>>= 7;
        if (value !== 0) byte |= 0x80;
        bytes.push(byte);
    } while (value !== 0);
    return bytes;
}

const section = (id, body) => [id, ...unsigned(body.length), ...body];
const module = (marker) => [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, ...section(0, [1, 0x78, marker])];

function component(...cores) {
    return Uint8Array.from([
        0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00,
        ...cores.flatMap((core) => section(1, core)),
        ...section(11, [1, 2, 3]),
    ]);
}

function memoryStore(initial = {}) {
    const files = new Map(Object.entries(initial));
    return {
        kind: "disk",
        files,
        async read(name) {
            return files.has(name) ? new Uint8Array(files.get(name)) : null;
        },
        async write(name, bytes) {
            files.set(name, new Uint8Array(bytes));
        },
        async readText(name) {
            return files.has(name) ? new TextDecoder().decode(files.get(name)) : null;
        },
        async writeText(name, text) {
            files.set(name, new TextEncoder().encode(text));
        },
        async remove(name) {
            files.delete(name);
        },
        async list() {
            return [...files].map(([name, bytes]) => ({ name, byteLength: bytes.byteLength }));
        },
    };
}

const realFetch = globalThis.fetch;
afterEach(() => {
    globalThis.fetch = realFetch;
});

describe("selectEngine", () => {
    it("takes the newest engine with a matching interface", () => {
        const entry = { engines: [engine("old", "0.8.0"), engine("new", "0.8.10"), engine("mid", "0.8.9")] };
        assert.equal(selectEngine(entry, INTERFACE).sha256, "new");
    });

    it("skips engines this SDK cannot run", () => {
        const entry = { engines: [engine("fits", "0.8.0"), engine("newer", "0.9.0", { interface: OTHER_INTERFACE })] };
        assert.equal(selectEngine(entry, INTERFACE).sha256, "fits");
    });

    it("prefers a release over its own release candidate", () => {
        const entry = { engines: [engine("candidate", "0.9.0rc2"), engine("release", "0.9.0")] };
        assert.equal(selectEngine(entry, INTERFACE).sha256, "release");
    });

    it("returns null without engines, or without a bundled fingerprint", () => {
        assert.equal(selectEngine({ id: "vsnap-base-256mb" }, INTERFACE), null);
        assert.equal(selectEngine({ engines: [engine("x", "0.8.0")] }, null), null);
    });
});

describe("coreModulesOf", () => {
    it("names the embedded modules the way jco names its files", () => {
        const modules = coreModulesOf(component(module(1), module(2), module(3)));

        assert.deepEqual(Object.keys(modules), ["vpod.core.wasm", "vpod.core2.wasm", "vpod.core3.wasm"]);
        assert.deepEqual([...modules["vpod.core2.wasm"]], module(2));
    });

    it("refuses a component without a core module", () => {
        assert.throws(() => coreModulesOf(component()), /no core module/);
    });

    it("refuses a truncated component", () => {
        const whole = component(module(1));
        assert.throws(() => coreModulesOf(whole.subarray(0, whole.length - 2)), /middle of a section/);
    });
});

describe("decompressEngine", () => {
    it("decodes a gzip download", async () => {
        const raw = component(module(1));
        assert.deepEqual(await decompressEngine(gzipSync(raw)), raw);
    });

    it("accepts a raw component", async () => {
        const raw = component(module(1));
        assert.deepEqual(await decompressEngine(raw), raw);
    });

    it("refuses bytes that are not wasm", async () => {
        await assert.rejects(decompressEngine(new TextEncoder().encode("<html>expired</html>")), /not a wasm/);
    });
});

describe("downloadEngine", () => {
    const bytes = gzipSync(component(module(7)));
    const listed = engine(sha256(bytes), "0.8.3");

    it("verifies and caches the download, readable afterwards", async () => {
        globalThis.fetch = async () => new Response(bytes);
        const store = memoryStore();

        await downloadEngine(store, "vsnap-x", listed, { registryUrl: REGISTRY });

        assert.deepEqual(await readCachedEngine(store, listed), bytes);
        assert.equal(await store.readText(`engine-${listed.sha256}.snapshot`), "vsnap-x");
    });

    it("stores nothing when the checksum does not match", async () => {
        globalThis.fetch = async () => new Response(new Uint8Array([1, 2, 3]));
        const store = memoryStore();

        await assert.rejects(downloadEngine(store, "vsnap-x", listed, { registryUrl: REGISTRY }), /checksum/);
        assert.equal(store.files.size, 0);
    });

    it("drops the engine it replaces for the same snapshot, and only that one", async () => {
        globalThis.fetch = async () => new Response(bytes);
        const replaced = "1".repeat(64);
        const unrelated = "2".repeat(64);
        const store = memoryStore({
            [`engine-${replaced}.download`]: new Uint8Array([1]),
            [`engine-${replaced}.sha256`]: new TextEncoder().encode(replaced),
            [`engine-${replaced}.snapshot`]: new TextEncoder().encode("vsnap-x"),
            [`engine-${unrelated}.download`]: new Uint8Array([1]),
            [`engine-${unrelated}.snapshot`]: new TextEncoder().encode("vsnap-y"),
        });

        await downloadEngine(store, "vsnap-x", listed, { registryUrl: REGISTRY });

        assert.equal(store.files.has(`engine-${replaced}.download`), false);
        assert.equal(store.files.has(`engine-${unrelated}.download`), true);
    });

    it("refreshes the catalogue once when a signed URL has expired", async () => {
        const requested = [];
        globalThis.fetch = async (url) => {
            requested.push(String(url));
            if (String(url) === REGISTRY) {
                return Response.json({
                    version: "1",
                    snapshots: [{ id: "vsnap-x", engines: [{ ...listed, url: "https://blob/fresh" }] }],
                });
            }
            return String(url) === "https://blob/fresh" ? new Response(bytes) : new Response(null, { status: 403 });
        };
        const store = memoryStore();

        await downloadEngine(store, "vsnap-x", listed, { registryUrl: REGISTRY });

        assert.deepEqual(requested, ["https://blob/e", REGISTRY, "https://blob/fresh"]);
        assert.deepEqual(await readCachedEngine(store, listed), bytes);
    });
});

describe("readCachedEngine", () => {
    it("drops a cached engine whose bytes no longer match", async () => {
        const listed = engine("f".repeat(64), "0.8.3");
        const store = memoryStore({
            [`engine-${listed.sha256}.download`]: new Uint8Array([9, 9, 9]),
            [`engine-${listed.sha256}.sha256`]: new TextEncoder().encode(listed.sha256),
        });

        assert.equal(await readCachedEngine(store, listed), null);
        assert.equal(store.files.size, 0);
    });
});

describe("snapshots.clear", () => {
    it("removes cached engines with the rest of the cache", async () => {
        const store = memoryStore({
            "engine-abc.download": new Uint8Array(10),
            "engine-abc.sha256": new TextEncoder().encode("abc"),
            "engine-abc.snapshot": new TextEncoder().encode("vsnap-x"),
        });

        await snapshots.clear({ store });

        assert.equal(store.files.size, 0);
    });
});
