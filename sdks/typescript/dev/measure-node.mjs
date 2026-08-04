#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { HOST_CORPUS_SCRIPT, summarize, timeGuest, workload } from "./measure-workload.js";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const { Sandbox, SandboxRuntime, createNodeWorkerTransport } = await import(
    join(packageRoot, "dist", "node", "index.js")
);

function parseArguments(argv) {
    const options = { apk: true };
    for (let index = 0; index < argv.length; index++) {
        if (argv[index] === "--snapshot") options.snapshot = argv[++index];
        else if (argv[index] === "--no-apk") options.apk = false;
    }
    return options;
}

function nativeSeconds(argv, tmp, repeats = 1) {
    if (spawnSync("which", [argv[0]]).status !== 0) {
        return null;
    }

    const times = [];
    for (let attempt = 0; attempt < repeats; attempt++) {
        const startedAt = performance.now();
        const result = spawnSync(argv[0], argv.slice(1), { cwd: tmp, timeout: 300_000 });
        if (result.status !== 0) {
            console.log(`    (native ${argv[0]} exited ${result.status}, no ratio)`);
            return null;
        }
        times.push((performance.now() - startedAt) / 1000);
    }

    times.sort((a, b) => a - b);
    return times[Math.floor(times.length / 2)];
}

async function main() {
    const options = parseArguments(process.argv.slice(2));
    const tmp = mkdtempSync(join(tmpdir(), "vpod-measure-"));

    const snapshot =
        options.snapshot === undefined
            ? "vsnap-base:latest"
            : existsSync(options.snapshot)
              ? { path: resolve(options.snapshot) }
              : options.snapshot;

    console.log(`snapshot: ${JSON.stringify(snapshot)}\n`);
    console.log("── floor (SDK overhead) ─────────────────────────────────────────");

    const rows = [];
    const say = (line) => console.log(line);

    if (typeof snapshot === "string") {
        const primer = new SandboxRuntime({ transport: await createNodeWorkerTransport() });
        await primer.ready();
        const pulled = await primer.pullSnapshot(snapshot);
        say(
            `    ${"snapshot pull".padEnd(30)} guest ${((pulled.fetchMilliseconds + pulled.verifyMilliseconds + pulled.storeMilliseconds) / 1000).toFixed(3)}s   ` +
                `source=${pulled.source} fetch=${(pulled.fetchMilliseconds / 1000).toFixed(3)}s ` +
                `verify=${(pulled.verifyMilliseconds / 1000).toFixed(3)}s store=${(pulled.storeMilliseconds / 1000).toFixed(3)}s`,
        );
    }

    let startedAt = performance.now();
    const box = await Sandbox.create({ snapshot });
    rows.push({
        label: "Sandbox.create() warm cache",
        guestSeconds: (performance.now() - startedAt) / 1000,
        nativeSeconds: null,
    });
    say(`    ${"Sandbox.create()".padEnd(30)} guest ${rows.at(-1).guestSeconds.toFixed(3)}s   lazy, no restore yet`);

    startedAt = performance.now();
    const first = await box.commands.run("echo ready");
    if (first.exitCode !== 0) {
        throw new Error(`the sandbox never came up: ${first.stderr}`);
    }
    rows.push({
        label: "first command (guest restore)",
        guestSeconds: (performance.now() - startedAt) / 1000,
        nativeSeconds: null,
    });
    say(`    ${"first command".padEnd(30)} guest ${rows.at(-1).guestSeconds.toFixed(3)}s   real cold start`);

    // Warm the prefork daemon and settle DNS before anything else is timed.
    await box.commands.run("python3 -c pass");
    await box.commands.run("echo warmup");

    let group = "floor";
    for (const step of workload()) {
        if (step.needs === "apk" && !options.apk) {
            say(`    ${step.label.padEnd(30)} skipped (--no-apk)`);
            continue;
        }

        if (step.group !== group) {
            group = step.group;
            say(`\n── ${group} ────────────────────────────────────────────────────`);
        }

        let guestSeconds;
        try {
            guestSeconds = await timeGuest(box, step, say);
        } catch (failure) {
            say(`    !! FAILED ${step.label}: ${failure.message}`);
            continue;
        }

        let native = null;
        if (step.native) {
            if (step.group === "ripgrep") {
                spawnSync("python3", ["-c", HOST_CORPUS_SCRIPT(`${tmp}/corpus`)]);
            }
            native = nativeSeconds(step.native(tmp), tmp, step.repeats ?? 1);
        }

        rows.push({ label: step.label, guestSeconds, nativeSeconds: native });
    }

    await box.close?.();
    rmSync(tmp, { recursive: true, force: true });

    console.log(summarize(rows));
}

await main();
process.exit(0);
