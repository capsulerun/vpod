import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { after, before, describe, it } from "node:test";

import { distPath, locateSnapshot, skipReason } from "../helpers.mjs";

const reason = skipReason();

async function online() {
    try {
        const response = await fetch("https://pypi.org/pypi/six/json", {
            signal: AbortSignal.timeout(8000),
        });
        return response.ok;
    } catch {
        return false;
    }
}

describe("node target", { skip: reason ?? false }, () => {
    let sandbox;
    let cacheDirectory;
    let hasNetwork = false;

    before(async () => {
        cacheDirectory = mkdtempSync(join(tmpdir(), "vpod-cache-"));
        hasNetwork = await online();

        const { Sandbox, createNodeTransport } = await import(distPath("node/index.js"));
        const snapshotPath = locateSnapshot();

        sandbox = await Sandbox.create({
            transport: await createNodeTransport({ cacheDirectory }),
            snapshotPath,
        });
    });

    after(async () => {
        await sandbox?.close();
        if (cacheDirectory !== undefined) {
            rmSync(cacheDirectory, { recursive: true, force: true });
        }
    });

    it("runs the guest", async () => {
        const result = await sandbox.commands.run("echo alive");

        assert.equal(result.stdout.trim(), "alive");
        assert.equal(result.exitCode, 0);
    });

    it("resolves a real hostname, not a synthetic one", async (t) => {
        if (!hasNetwork) {
            t.skip("no internet");
            return;
        }

        const result = await sandbox.commands.run("getent hosts pypi.org", { timeout: 30 });

        assert.match(result.stdout, /pypi\.org/);
        assert.doesNotMatch(
            result.stdout,
            /198\.18\./,
            "198.18/15 is the browser's synthetic range; Node should resolve for real",
        );
    });

    it("reaches an ordinary https host with no CORS in the way", async (t) => {
        if (!hasNetwork) {
            t.skip("no internet");
            return;
        }

        const result = await sandbox.commands.run(
            "wget -q -O- https://pypi.org/pypi/six/json | head -c 40",
            { timeout: 60 },
        );

        assert.equal(result.exitCode, 0, result.stderr);
        assert.match(result.stdout, /"info"/);
    });

    it("reaches a host with no CORS headers at all, which a browser cannot", async (t) => {
        if (!hasNetwork) {
            t.skip("no internet");
            return;
        }

        const result = await sandbox.commands.run("apk update 2>&1 | tail -1", { timeout: 180 });

        assert.match(result.stdout, /packages available/);
    });

    it("refuses the browser's fetch transport, which Node has no use for", async () => {
        const { createNodeTransport } = await import(distPath("node/index.js"));
        const transport = await createNodeTransport({ cacheDirectory });

        await assert.rejects(() => transport.call({ kind: "enable-network" }), /node:net/);
        transport.terminate();
    });

    it("keeps the calling thread responsive while the guest runs", async () => {
        const { Sandbox, createNodeWorkerTransport } = await import(distPath("node/index.js"));
        const snapshotPath = locateSnapshot();

        const threaded = await Sandbox.create({
            transport: await createNodeWorkerTransport({ cacheDirectory }),
            snapshotPath,
        });

        try {
            let ticks = 0;
            let worstGap = 0;
            let previous = performance.now();
            const timer = setInterval(() => {
                const now = performance.now();
                worstGap = Math.max(worstGap, now - previous);
                previous = now;
                ticks++;
            }, 10);

            const startedAt = performance.now();
            const result = await threaded.code.run(
                "s=0\nfor i in range(400000): s=(s+i*i)^(i&0xff)\nprint(s)",
                { timeout: 120 },
            );
            const elapsed = performance.now() - startedAt;
            clearInterval(timer);

            assert.match(result.text.trim(), /^\d+$/, `guest did not compute: ${result.error}`);
            assert.ok(elapsed > 300, `too quick to prove anything: ${elapsed.toFixed(0)}ms`);

            const expectedTicks = Math.floor(elapsed / 30);
            assert.ok(
                ticks >= expectedTicks,
                `${ticks} timer ticks in ${elapsed.toFixed(0)}ms, expected at least ${expectedTicks}: the event loop was blocked`,
            );
            assert.ok(
                worstGap < 500,
                `worst timer gap was ${worstGap.toFixed(0)}ms, so the guest stalled the loop`,
            );
        } finally {
            await threaded.close();
        }
    });

    it("caches a snapshot on disk where the Python SDK looks for it", async () => {
        const { defaultCacheDirectory } = await import(distPath("node/index.js"));
        const directory = defaultCacheDirectory();

        assert.match(directory, /vpod[/\\]snapshots$/);
    });
});
