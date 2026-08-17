import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { basename } from "node:path";
import { after, before, describe, it } from "node:test";

import { distPath, loadSdk, locateSnapshot, skipReason } from "../helpers.mjs";
import {
    BASELINE_NAME,
    GUEST_WORKLOADS,
    WALL_CEILINGS,
    baselineUrl,
    entryFor,
    guestProgram,
    loadBaseline,
    withinTolerance,
} from "./workloads.mjs";

const reason = skipReason();

function builtTier() {
    try {
        return JSON.parse(readFileSync(distPath("component/manifest.json"), "utf8")).tier;
    } catch {
        return "unknown";
    }
}


function snapshotIdentity() {
    const path = locateSnapshot();
    if (path === null) {
        return { name: null, sha256: null };
    }
    return {
        name: basename(path),
        sha256: createHash("sha256").update(readFileSync(path)).digest("hex"),
    };
}

const snapshot = snapshotIdentity();

const recorded = entryFor(await loadBaseline(), snapshot.sha256);
const strict = recorded !== null;

describe("performance", { skip: reason ?? false }, () => {
    let sandbox;
    let worker;
    const report = {
        snapshot: snapshot.name,
        snapshotSha256: snapshot.sha256,
        strict,
        guest: {},
        wall: {},
    };

    before(async () => {
        const cli = await import(distPath("shims/cli.js"));
        cli._setEnv({ VPOD_HOST_TLS: "1" });

        const { Sandbox, createInlineTransport } = await loadSdk();
        const snapshotPath = locateSnapshot();

        const startedAt = performance.now();
        sandbox = await Sandbox.create({
            transport: await createInlineTransport(),
            snapshot: {
                bytes: readFileSync(snapshotPath),
                name: basename(snapshotPath),
            },
        });
        report.wall.bootSeconds = (performance.now() - startedAt) / 1000;

        await sandbox.code.run("print('warm')", { timeout: 120 });
    });

    after(async () => {
        await sandbox?.close();
        await worker?.terminate();

        const json = `${JSON.stringify(report, null, 2)}\n`;
        if (process.env.VPOD_PERF_OUTPUT) {
            writeFileSync(process.env.VPOD_PERF_OUTPUT, json);
        }
        console.log(json);
    });

    it("compares against the snapshot the constants were recorded from", () => {
        if (!strict && process.env.VPOD_PERF_REQUIRE_STRICT === "1") {
            assert.fail(
                `the snapshot is ${snapshot.sha256?.slice(0, 16) ?? "missing"}, and ` +
                    `${baselineUrl() ?? "no baseline URL"} has no timings for those bytes. ` +
                    `Either the channel was republished without re-recording, or this is a ` +
                    `different image. Re-record with VPOD_PERF_RECORD=1 and upload ` +
                    `${BASELINE_NAME} alongside the snapshots.`,
            );
        }
        assert.ok(true);
    });

    it("boots within its ceiling", () => {
        assert.ok(
            report.wall.bootSeconds < WALL_CEILINGS.bootSeconds,
            `boot took ${report.wall.bootSeconds.toFixed(2)}s, ceiling ${WALL_CEILINGS.bootSeconds}s`,
        );
    });

    for (const [name, workload] of Object.entries(GUEST_WORKLOADS)) {
        it(`runs the ${name} workload without drifting`, async () => {
            const program = guestProgram(workload.body);

            await sandbox.code.run(program, { timeout: 300 });

            const startedAt = performance.now();
            const result = await sandbox.code.run(program, { timeout: 300 });
            const wallSeconds = (performance.now() - startedAt) / 1000;

            assert.ok(result.success, `${name} failed in the guest: ${result.error}`);
            const guestSeconds = Number(result.text.trim());
            assert.ok(Number.isFinite(guestSeconds), `${name} reported ${result.text}`);

            const expected = recorded?.guestSeconds?.[name];
            report.guest[name] = {
                guestSeconds,
                wallSeconds,
                throughput: guestSeconds / wallSeconds,
                expected,
            };

            if (expected === undefined) return;

            assert.ok(
                withinTolerance(guestSeconds, expected, workload.tolerance),
                `${name} guest time is ${guestSeconds.toFixed(6)}s, the channel recorded ` +
                    `${expected}s for these exact bytes (tolerance ` +
                    `${workload.tolerance * 100}%). Guest time is deterministic, so this is ` +
                    `the guest doing a different amount of work, not the host running ` +
                    `slower. If the change is intended, re-record with VPOD_PERF_RECORD=1 ` +
                    `and republish ${BASELINE_NAME}.`,
            );
        });
    }

    it("keeps emulator throughput above its floor", () => {
        const measured = Object.values(report.guest);
        assert.ok(measured.length > 0, "no workload ran");

        const best = Math.max(...measured.map((entry) => entry.throughput));
        report.wall.throughput = best;

        const tier = builtTier();
        const floor =
            tier === "aot" ? WALL_CEILINGS.throughputFloorAot : WALL_CEILINGS.throughputFloor;
        report.wall.tier = tier;
        report.wall.throughputFloor = floor;

        assert.ok(
            best > floor,
            `throughput is ${best.toFixed(2)}x guest-seconds per wall-second, floor ` +
                `${floor}x for the ${tier} tier. Either the emulator regressed badly ` +
                `or this runner is far slower than any seen before.`,
        );
    });

    it("keeps per-command overhead low", async () => {
        const rounds = 10;
        const startedAt = performance.now();
        for (let index = 0; index < rounds; index++) {
            await sandbox.commands.run("echo x", { timeout: 60 });
        }
        const perCall = (performance.now() - startedAt) / rounds / 1000;
        report.wall.shellPerCallSeconds = perCall;

        assert.ok(
            perCall < WALL_CEILINGS.shellPerCallSeconds,
            `a trivial command costs ${perCall.toFixed(3)}s, ceiling ` +
                `${WALL_CEILINGS.shellPerCallSeconds}s`,
        );
    });

    it("completes a network round trip within its ceiling", async () => {
        const { setSocketBackend } = await loadSdk();
        const { FetchSocketBackend } = await import(distPath("net/fetch-backend.js"));
        const cli = await import(distPath("shims/cli.js"));
        const { Worker } = await import("node:worker_threads");
        const { resolve, dirname } = await import("node:path");
        const { fileURLToPath } = await import("node:url");

        const here = dirname(fileURLToPath(import.meta.url));
        const script = `() => async (url) =>
            new Response("ok\\n", { status: 200, headers: { "content-type": "text/plain" } })`;

        worker = new Worker(resolve(here, "..", "helpers", "fetch-driver-worker.mjs"), {
            workerData: { driverModule: distPath("net/fetch-driver.js"), script },
        });
        await new Promise((settle, fail) => {
            worker.once("message", settle);
            worker.once("error", fail);
        });

        cli._setEnv({ VPOD_HOST_TLS: "1" });
        setSocketBackend(new FetchSocketBackend((command) => worker.postMessage(command)));

        const { Sandbox, createInlineTransport } = await loadSdk();
        const snapshotPath = locateSnapshot();
        const networked = await Sandbox.create({
            transport: await createInlineTransport(),
            snapshot: {
                bytes: readFileSync(snapshotPath),
                name: basename(snapshotPath),
            },
        });

        try {
            await networked.commands.run("wget -q -O- https://warm.test/ 2>&1", {
                timeout: 120,
            });

            const startedAt = performance.now();
            const result = await networked.commands.run("wget -q -O- https://perf.test/ 2>&1", {
                timeout: 120,
            });
            const seconds = (performance.now() - startedAt) / 1000;
            report.wall.networkRoundTripSeconds = seconds;

            assert.match(result.stdout, /ok/, "the canned upstream did not answer");
            assert.ok(
                seconds < WALL_CEILINGS.networkRoundTripSeconds,
                `a round trip took ${seconds.toFixed(2)}s, ceiling ` +
                    `${WALL_CEILINGS.networkRoundTripSeconds}s`,
            );
        } finally {
            await networked.close();
        }
    });
});
