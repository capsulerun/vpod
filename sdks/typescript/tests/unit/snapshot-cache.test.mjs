import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { mkdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";

import { distPath } from "../helpers.mjs";

const { snapshots } = await import(distPath("index.js"));
const { FileSnapshotStore } = await import(distPath("node/index.js"));
const { cached, clear } = snapshots;

function fakeStore(files) {
    const held = new Map(files.map(({ name, byteLength }) => [name, byteLength]));
    return {
        kind: "opfs",
        removed: [],
        async list() {
            return [...held].map(([name, byteLength]) => ({ name, byteLength }));
        },
        async remove(name) {
            this.removed.push(name);
            held.delete(name);
        },
        async read() {
            return null;
        },
        async write() {},
        async readText() {
            return null;
        },
        async writeText() {},
    };
}

const POPULATED = () =>
    fakeStore([
        { name: "vsnap-base-256mb.snap", byteLength: 268_000_000 },
        { name: "vsnap-base-256mb.sha256", byteLength: 64 },
        { name: "alpine-3.23.0-256mb.snap", byteLength: 268_000_000 },
        { name: "alpine-3.23.0-256mb.sha256", byteLength: 64 },
        { name: "catalogue-1a2b3c4d.json", byteLength: 2000 },
        { name: "catalogue-1a2b3c4d.fetched-at", byteLength: 13 },
        { name: "9f8e-7d6c.delta", byteLength: 2_100_000 },
        { name: "instances.json", byteLength: 180 },
    ]);

describe("cached", () => {
    it("reports snapshots with their sizes", async () => {
        const listed = await cached({ store: POPULATED() });

        assert.deepEqual(
            listed.map((entry) => entry.id).sort(),
            ["alpine-3.23.0-256mb", "vsnap-base-256mb"],
        );
        assert.equal(listed[0].byteLength, 268_000_000);
    });

    it("counts only snapshots, not the bookkeeping beside them", async () => {
        assert.equal((await cached({ store: POPULATED() })).length, 2);
    });

    it("is empty rather than throwing where there is no store", async () => {
        assert.deepEqual(await cached({ store: null }), []);
    });
});

describe("clear", () => {
    it("drops snapshots, digests and the catalogue", async () => {
        const store = POPULATED();
        await clear({ store });

        assert.ok(store.removed.includes("vsnap-base-256mb.snap"));
        assert.ok(store.removed.includes("vsnap-base-256mb.sha256"));
        assert.ok(store.removed.includes("catalogue-1a2b3c4d.json"));
        assert.ok(store.removed.includes("catalogue-1a2b3c4d.fetched-at"));
        assert.deepEqual(await cached({ store }), []);
    });

    it("keeps suspended instances by default", async () => {
        const store = POPULATED();
        await clear({ store });

        assert.ok(!store.removed.includes("9f8e-7d6c.delta"));
        assert.ok(!store.removed.includes("instances.json"));
    });

    it("drops them when asked", async () => {
        const store = POPULATED();
        await clear({ store, instances: true });

        assert.ok(store.removed.includes("9f8e-7d6c.delta"));
        assert.ok(store.removed.includes("instances.json"));
    });

    it("reports the bytes it reclaimed", async () => {
        const reclaimed = await clear({ store: POPULATED() });
        assert.equal(reclaimed, 268_000_000 * 2 + 64 * 2 + 2000 + 13);
    });

    it("counts instance bytes only when it removes them", async () => {
        const withInstances = await clear({ store: POPULATED(), instances: true });
        const without = await clear({ store: POPULATED() });
        assert.equal(withInstances - without, 2_100_000 + 180);
    });

    it("reclaims nothing where there is no store", async () => {
        assert.equal(await clear({ store: null }), 0);
    });

    it("leaves anything it does not recognise alone", async () => {
        const store = fakeStore([{ name: "something-else.txt", byteLength: 10 }]);
        await clear({ store, instances: true });
        assert.deepEqual(store.removed, []);
    });

    it("keeps a snapshot it did not download, having no digest for it", async () => {
        const store = fakeStore([
            { name: "built-locally.snap", byteLength: 268_000_000 },
            { name: "built-locally.meta", byteLength: 64 },
        ]);
        const reclaimed = await clear({ store });

        assert.deepEqual(store.removed, []);
        assert.equal(reclaimed, 0);
    });

    it("drops a catalogue written by an older version", async () => {
        const store = fakeStore([
            { name: "catalogue.json", byteLength: 2000 },
            { name: "catalogue.fetched-at", byteLength: 13 },
        ]);
        await clear({ store });

        assert.deepEqual(store.removed.sort(), ["catalogue.fetched-at", "catalogue.json"]);
    });
});

describe("the disk store", () => {
    async function populated() {
        const directory = mkdtempSync(join(tmpdir(), "vpod-cache-"));
        await writeFile(join(directory, "vsnap-base-256mb.snap"), Buffer.alloc(8192));
        await writeFile(join(directory, "vsnap-base-256mb.sha256"), "a".repeat(64));
        await writeFile(join(directory, "catalogue-1a2b3c4d.json"), "{}");

        await writeFile(join(directory, "alpine-3.23.0-256mb.snap"), Buffer.alloc(4096));
        await writeFile(join(directory, "alpine-3.23.0-256mb.meta"), "b".repeat(64));
        await mkdir(join(directory, "a-directory"));
        return { directory, store: new FileSnapshotStore(directory) };
    }

    it("lists files with the sizes they occupy", async () => {
        const { directory, store } = await populated();
        try {
            const listed = await store.list();
            const byName = Object.fromEntries(listed.map((file) => [file.name, file.byteLength]));

            assert.equal(byName["vsnap-base-256mb.snap"], 8192);
            assert.equal(byName["vsnap-base-256mb.sha256"], 64);
        } finally {
            rmSync(directory, { recursive: true, force: true });
        }
    });

    it("lists files only, never the directories beside them", async () => {
        const { directory, store } = await populated();
        try {
            const listed = await store.list();
            assert.ok(!listed.some((file) => file.name === "a-directory"));
        } finally {
            rmSync(directory, { recursive: true, force: true });
        }
    });

    it("is empty rather than throwing where nothing has been cached yet", async () => {
        const store = new FileSnapshotStore(join(tmpdir(), "vpod-cache-never-created"));
        assert.deepEqual(await store.list(), []);
    });

    it("clears through to the filesystem", async () => {
        const { directory, store } = await populated();
        try {
            const reclaimed = await clear({ store });

            assert.equal(reclaimed, 8192 + 64 + 2);
            assert.deepEqual(
                (await store.list()).map((file) => file.name).sort(),
                ["alpine-3.23.0-256mb.meta", "alpine-3.23.0-256mb.snap"],
            );
        } finally {
            rmSync(directory, { recursive: true, force: true });
        }
    });

    it("leaves a snapshot another vpod tool put in the shared directory", async () => {
        const { directory, store } = await populated();
        try {
            await clear({ store });
            assert.deepEqual(
                (await cached({ store })).map((entry) => entry.id),
                ["alpine-3.23.0-256mb"],
            );
        } finally {
            rmSync(directory, { recursive: true, force: true });
        }
    });
});
