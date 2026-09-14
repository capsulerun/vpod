/**
 * A snapshot with its own engine, end to end in Node, against a local catalogue.
 */

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, describe, it } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";
import { gzipSync } from "node:zlib";

import { distPath, locateSnapshot, skipReason } from "../helpers.mjs";

const manifest = existsSync(distPath("component/manifest.json"))
    ? JSON.parse(readFileSync(distPath("component/manifest.json"), "utf8"))
    : {};
const engineComponent = new URL(`../../${manifest.source ?? "wasm/missing"}`, import.meta.url);

const skip =
    skipReason() ??
    (manifest.engineInterface ? null : "the build did not record an engine interface") ??
    (existsSync(engineComponent) ? null : "the bundled engine component is not on disk");

describe("a snapshot's own engine", { skip: skip ?? false }, () => {
    let server;
    let registryUrl;
    let cacheDirectory;
    let engineSha256;
    let Sandbox;

    before(async () => {
        cacheDirectory = mkdtempSync(join(tmpdir(), "vpod-engines-"));
        process.env.VPOD_CACHE_DIR = cacheDirectory;

        const snapshotBytes = readFileSync(locateSnapshot());
        const engineBytes = gzipSync(readFileSync(engineComponent));
        engineSha256 = createHash("sha256").update(engineBytes).digest("hex");

        server = createServer((request, response) => {
            if (request.url === "/snapshots.json") {
                response.setHeader("content-type", "application/json");
                response.end(JSON.stringify({
                    version: "1",
                    snapshots: [{
                        id: "vsnap-imagetest-256mb",
                        name: "image-test",
                        tag: "latest",
                        memory_label: "256mb",
                        description: "per-image engine integration test",
                        url: "/snapshot.snap",
                        sha256: createHash("sha256").update(snapshotBytes).digest("hex"),
                        size: snapshotBytes.byteLength,
                        engines: [{
                            vpod_version: "0.8.3",
                            interface: manifest.engineInterface,
                            url: "/engine.wasm.gz",
                            sha256: engineSha256,
                            size: engineBytes.byteLength,
                        }],
                    }],
                }));
                return;
            }
            response.end(request.url === "/snapshot.snap" ? snapshotBytes : engineBytes);
        });
        await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
        registryUrl = `http://127.0.0.1:${server.address().port}/snapshots.json`;

        ({ Sandbox } = await import(distPath("node/index.js")));
    });

    after(() => {
        server?.close();
        delete process.env.VPOD_CACHE_DIR;
        rmSync(cacheDirectory, { recursive: true, force: true });
    });

    it("starts the first sandbox on the bundled engine and the next on the image engine", async () => {
        const first = await Sandbox.create({ snapshot: "image-test", registryUrl });
        assert.equal(first.tier, manifest.tier);
        assert.match((await first.commands.run("echo one")).stdout, /one/);
        await first.close();

        const cached = join(cacheDirectory, `engine-${engineSha256}.sha256`);
        for (let waited = 0; !existsSync(cached); waited += 100) {
            assert.ok(waited < 60_000, "the engine was never cached");
            await sleep(100);
        }

        // Suspend and resume are not covered here: suspend already fails on the
        // Node worker transport with the bundled engine, before this feature.
        const second = await Sandbox.create({ snapshot: "image-test", registryUrl });
        assert.equal(second.tier, "image");
        assert.match((await second.commands.run("echo two && uname -m")).stdout, /two\s+riscv64/);
        await second.close();

        const bundled = await Sandbox.create({ snapshot: "image-test", registryUrl, engine: "default" });
        assert.equal(bundled.tier, manifest.tier);
        assert.match((await bundled.commands.run("echo three")).stdout, /three/);
        await bundled.close();
    });
});
