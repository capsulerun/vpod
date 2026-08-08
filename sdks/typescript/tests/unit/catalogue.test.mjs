import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { distPath } from "../helpers.mjs";

const { snapshots } = await import(distPath("index.js"));
const { resolveSnapshot } = snapshots;

const entry = (id, name, tag) => ({
    id,
    name,
    tag,
    memory_label: "256MB",
    description: "",
    url: `https://example.invalid/${id}.snap`,
    sha256: "0".repeat(64),
    size: 1,
});

const CATALOGUE = [
    entry("alpine-3.23.0-256mb", "alpine", "3.23.0"),
    entry("vsnap-base-256mb", "vsnap-base", "1.0.0"),
    entry("vsnap-data-512mb", "vsnap-data", "1.0.0"),
];

describe("resolveSnapshot", () => {
    it("matches a bare id", () => {
        assert.equal(resolveSnapshot(CATALOGUE, "vsnap-data-512mb").id, "vsnap-data-512mb");
    });

    it("matches name and tag", () => {
        assert.equal(resolveSnapshot(CATALOGUE, "alpine:3.23.0").id, "alpine-3.23.0-256mb");
    });

    it("treats latest as any tag", () => {
        assert.equal(resolveSnapshot(CATALOGUE, "vsnap-base:latest").id, "vsnap-base-256mb");
    });

    it("defaults a bare name to latest", () => {
        assert.equal(resolveSnapshot(CATALOGUE, "vsnap-base").id, "vsnap-base-256mb");
    });

    it("treats an empty tag as latest", () => {
        assert.equal(resolveSnapshot(CATALOGUE, "alpine:").id, "alpine-3.23.0-256mb");
    });

    it("returns the first match when several tags qualify", () => {
        const catalogue = [entry("a-256mb", "dup", "1.0.0"), entry("b-256mb", "dup", "2.0.0")];
        assert.equal(resolveSnapshot(catalogue, "dup:latest").id, "a-256mb");
    });

    it("rejects a tag that does not exist", () => {
        assert.throws(() => resolveSnapshot(CATALOGUE, "alpine:9.9.9"), /not found/);
    });

    it("lists what is available when nothing matches", () => {
        assert.throws(
            () => resolveSnapshot(CATALOGUE, "nope"),
            /alpine:3\.23\.0, vsnap-base:1\.0\.0, vsnap-data:1\.0\.0/,
        );
    });
});

describe("fetchCatalogue when the cache is busy", () => {
    function lockedStore() {
        const attempts = [];
        return {
            kind: "opfs",
            attempts,
            async list() {
                return [];
            },
            async remove() {},
            async read() {
                return null;
            },
            async write() {},
            async readText() {
                return null;
            },
            async writeText(name) {
                attempts.push(name);
                throw new DOMException(
                    "Access Handles cannot be created if there is another open Access Handle",
                    "NoModificationAllowedError",
                );
            },
        };
    }

    const CATALOGUE_BODY = { snapshots: [entry("vsnap-base-256mb", "vsnap-base", "1.0.0")] };

    it("returns the catalogue even though it could not be cached", async (t) => {
        t.mock.method(globalThis, "fetch", async () =>
            new Response(JSON.stringify(CATALOGUE_BODY), {
                status: 200,
                headers: { "Content-Type": "application/json" },
            }));

        const store = lockedStore();
        const catalogue = await snapshots.fetchCatalogue(store, {
            registryUrl: "https://example.invalid/snapshots.json",
        });

        assert.equal(catalogue.snapshots.length, 1);
        assert.ok(store.attempts.length > 0, "it should have tried to cache the catalogue");
    });
});
