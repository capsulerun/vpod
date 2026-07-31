import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { existsSync, mkdtempSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { after, before, describe, it } from "node:test";

const run = promisify(execFile);

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const nodeEntry = join(packageRoot, "dist", "node", "index.js");
const distMissing = !existsSync(nodeEntry);

const SNAPSHOT_BODY = Buffer.from("not a real snapshot, and never loaded by one\n");

describe("disk cache", { skip: distMissing ? "dist/ is missing: run npm run build" : false }, () => {
    let server;
    let origin;
    let cacheDirectory;
    let workspace;
    const hits = [];

    before(async () => {
        cacheDirectory = mkdtempSync(join(tmpdir(), "vpod-cache-"));
        workspace = mkdtempSync(join(tmpdir(), "vpod-pull-"));

        server = createServer((request, response) => {
            hits.push(request.url);

            if (request.url === "/snapshots.json") {
                response.writeHead(200, { "content-type": "application/json" });
                response.end(
                    JSON.stringify({
                        version: "1",
                        snapshots: [
                            {
                                id: "cache-probe-1mb",
                                name: "cache-probe",
                                tag: "1.0.0",
                                memory_label: "1mb",
                                description: "a local stand-in, so this test needs no internet",
                                url: `${origin}/cache-probe.snap`,
                                sha256: createHash("sha256").update(SNAPSHOT_BODY).digest("hex"),
                                size: SNAPSHOT_BODY.byteLength,
                            },
                        ],
                    }),
                );
                return;
            }

            if (request.url === "/cache-probe.snap") {
                response.writeHead(200, { "content-type": "application/octet-stream" });
                response.end(SNAPSHOT_BODY);
                return;
            }

            response.writeHead(404);
            response.end();
        });

        await new Promise((settle) => server.listen(0, "127.0.0.1", settle));
        origin = `http://127.0.0.1:${server.address().port}`;

        writeFileSync(
            join(workspace, "pull.mjs"),
            `
            const { createNodeTransport } = await import(${JSON.stringify(nodeEntry)});
            const transport = await createNodeTransport({
                cacheDirectory: process.argv[2],
            });
            const pulled = await transport.call({
                kind: "pull-snapshot",
                name: "cache-probe",
                registryUrl: process.argv[3],
            });
            transport.terminate();
            console.log(JSON.stringify(pulled));
            `,
        );
    });

    after(async () => {
        if (server !== undefined) {
            await new Promise((settle) => server.close(settle));
        }
        for (const directory of [cacheDirectory, workspace]) {
            if (directory !== undefined) {
                rmSync(directory, { recursive: true, force: true });
            }
        }
    });

    async function pullInFreshProcess() {
        const { stdout } = await run(
            process.execPath,
            [join(workspace, "pull.mjs"), cacheDirectory, `${origin}/snapshots.json`],
            { encoding: "utf8" },
        );
        return JSON.parse(stdout);
    }

    it("fetches once, then starts from disk in a second process", async () => {
        const first = await pullInFreshProcess();
        assert.equal(first.source, "network");
        assert.equal(first.byteLength, SNAPSHOT_BODY.byteLength);

        const downloads = hits.filter((path) => path === "/cache-probe.snap").length;
        assert.equal(downloads, 1, `expected one download, saw ${downloads}`);

        const second = await pullInFreshProcess();
        assert.equal(
            second.source,
            "disk",
            "the second process refetched rather than reading the cache",
        );
        assert.deepEqual(
            hits.filter((path) => path === "/cache-probe.snap"),
            ["/cache-probe.snap"],
            "the second process hit the network for the snapshot body",
        );

        assert.deepEqual(
            hits.filter((path) => path === "/snapshots.json"),
            ["/snapshots.json"],
            "the second process refetched the catalogue",
        );
    });

    it("hands the guest a path into the cache, not a copy", async () => {
        const pulled = await pullInFreshProcess();

        assert.equal(pulled.snapshotPath, join(cacheDirectory, "cache-probe-1mb.snap"));
        assert.equal(statSync(pulled.snapshotPath).size, SNAPSHOT_BODY.byteLength);
    });

    it("refetches when the cached bytes no longer match the digest", async () => {
        writeFileSync(join(cacheDirectory, "cache-probe-1mb.snap"), "corrupted");

        const pulled = await pullInFreshProcess();

        assert.equal(pulled.source, "network");
        assert.equal(
            hits.filter((path) => path === "/cache-probe.snap").length,
            2,
            "a corrupted cache entry was served rather than replaced",
        );
    });
});
