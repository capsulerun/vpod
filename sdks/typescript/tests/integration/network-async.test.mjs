import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { Worker } from "node:worker_threads";
import { after, before, describe, it } from "node:test";

import { distPath, loadSdk, locateSnapshot, skipReason } from "../helpers.mjs";

const reason = skipReason();

const { setSocketBackend } = await loadSdk();
const { FetchSocketBackend } = await import(distPath("net/fetch-backend.js"));
const cli = await import(distPath("shims/cli.js"));

const workerPath = resolve(
    dirname(fileURLToPath(import.meta.url)),
    "..",
    "helpers",
    "fetch-driver-worker.mjs",
);

function delayedFetchSource(delayMilliseconds, refusedHosts) {
    return `() => {
        const refused = new Set(${JSON.stringify(refusedHosts)});
        return async (url) => {
            await new Promise((settle) => setTimeout(settle, ${delayMilliseconds}));
            const host = new URL(url).hostname;
            if (refused.has(host)) {
                throw new TypeError("Failed to fetch");
            }
            return new Response("hello from " + host + "\\n", {
                status: 200,
                headers: { "content-type": "text/plain" },
            });
        };
    }`;
}

async function startDriver(script, options = {}) {
    const worker = new Worker(workerPath, {
        workerData: { driverModule: distPath("net/fetch-driver.js"), script, options },
    });

    await new Promise((settle, fail) => {
        worker.once("message", settle);
        worker.once("error", fail);
    });

    const backend = new FetchSocketBackend((command) => worker.postMessage(command));
    return { worker, backend };
}

describe("network transport, answered from another thread", { skip: reason ?? false }, () => {
    let sandbox;
    let worker;

    before(async () => {
        cli._setEnv({ VPOD_HOST_TLS: "1" });

        const started = await startDriver(delayedFetchSource(150, ["refused.test"]));
        worker = started.worker;
        setSocketBackend(started.backend);

        const { Sandbox, createInlineTransport } = await loadSdk();
        const snapshotPath = locateSnapshot();

        sandbox = await Sandbox.create({
            transport: await createInlineTransport(),
            snapshotBytes: readFileSync(snapshotPath),
            snapshotName: basename(snapshotPath),
        });
    });

    after(async () => {
        await sandbox?.close();
        await worker?.terminate();
    });

    it("delivers a reply that lands after the guest has parked", async () => {
        const result = await sandbox.commands.run(
            "wget -q -O- https://slow.test/ 2>&1",
            { timeout: 60 },
        );

        assert.equal(result.exitCode, 0, result.stdout + result.stderr);
        assert.match(result.stdout, /hello from slow\.test/);
    });

    it("closes a late reply, so a pipe reading it sees end-of-input", async () => {
        const result = await sandbox.commands.run(
            "wget -q -O- https://piped.test/ | head -c 200",
            { timeout: 60 },
        );

        assert.equal(
            result.exitCode,
            0,
            `the pipeline never finished: a late reply is never closed. ${result.stdout}`,
        );
        assert.match(result.stdout, /hello from piped\.test/);
    });

    it("closes a refused connection that fails after the guest has parked", async () => {
        const result = await sandbox.commands.run(
            "wget -q -O- https://refused.test/ | head -c 200",
            { timeout: 60 },
        );

        assert.notEqual(
            result.exitCode,
            124,
            "a refused connection is never closed, so the pipe reader waits forever",
        );
    });

    async function leakedBy(command) {
        const count = async () => {
            const seen = await sandbox.commands.run("ps | grep -c '[s]sl_client'", {
                timeout: 30,
            });
            return Number(seen.stdout.trim());
        };

        const before = await count();
        await sandbox.commands.run(command, { timeout: 60 });

        for (let index = 0; index < 3; index++) {
            await sandbox.commands.run("true", { timeout: 30 });
        }

        return (await count()) - before;
    }

    it("leaves no ssl_client behind after a refusal", async () => {
        const leaked = await leakedBy("wget -q -O- https://refused.test/ | head -c 200");

        assert.equal(leaked, 0, `${leaked} ssl_client survived a refusal`);
    });

    it("leaves no ssl_client behind after a success", async () => {
        const leaked = await leakedBy("wget -q -O- https://works.test/ | head -c 200");

        assert.equal(leaked, 0, `${leaked} ssl_client survived a successful request`);
    });

    it("keeps replies in order on one keep-alive connection", async () => {
        const result = await sandbox.commands.run(
            "wget -q -O- https://a.test/ https://b.test/ 2>&1",
            { timeout: 60 },
        );

        const order = result.stdout.match(/hello from ([a-z.]+)/g) ?? [];
        assert.deepEqual(order, ["hello from a.test", "hello from b.test"]);
    });
});
