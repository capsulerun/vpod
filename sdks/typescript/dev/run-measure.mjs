#!/usr/bin/env node

import { spawn, spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { HOST_CORPUS_SCRIPT, summarize, workload } from "./measure-workload.js";
import { browserArguments, locateBrowser } from "./browsers.mjs";
import { startServer } from "./serve.mjs";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const resultsDir = join(packageRoot, "dev", "results");


function parseArguments(argv) {
    const options = { browser: "chrome", port: 8794, timeoutSeconds: 900, attach: false };
    for (let index = 0; index < argv.length; index++) {
        const flag = argv[index];
        if (flag === "--browser") options.browser = argv[++index];
        else if (flag === "--port") options.port = Number(argv[++index]);
        else if (flag === "--timeout") options.timeoutSeconds = Number(argv[++index]);
        else if (flag === "--attach") options.attach = true;
        // --name for parity with the other runners, which all spell it that way.
        else if (flag === "--snapshot" || flag === "--name") options.snapshot = argv[++index];
        else if (flag === "--apk") options.apk = argv[++index];
        else if (flag === "--snapshot-dir") options.snapshotDir = argv[++index];
    }
    return options;
}

function nativeSeconds(argv, tmp, repeats) {
    if (spawnSync("which", [argv[0]]).status !== 0) {
        return null;
    }

    const times = [];
    for (let attempt = 0; attempt < repeats; attempt++) {
        const startedAt = performance.now();
        const result = spawnSync(argv[0], argv.slice(1), { cwd: tmp, timeout: 300_000 });
        if (result.status !== 0) {
            return null;
        }
        times.push((performance.now() - startedAt) / 1000);
    }

    times.sort((a, b) => a - b);
    return times[Math.floor(times.length / 2)];
}

async function main() {
    const options = parseArguments(process.argv.slice(2));
    mkdirSync(resultsDir, { recursive: true });

    let resolveResult;
    const pendingResult = new Promise((done) => {
        resolveResult = done;
    });

    const running = await startServer({
        port: options.port,
        isolate: true,
        snapshotDir: options.snapshotDir,
        onResult: (body) => resolveResult(JSON.parse(body)),
    });

    const parameters = new URLSearchParams();
    if (options.snapshot) parameters.set("name", options.snapshot);
    if (options.apk) parameters.set("apk", options.apk);

    const query = parameters.size > 0 ? `?${parameters}` : "";
    const pageUrl = `${running.url}dev/measure.html${query}`;
    console.log(`serving ${pageUrl} (snapshots from ${running.snapshots}, COOP/COEP on)\n`);

    let child;
    if (options.attach) {
        console.log("open the URL above; waiting for its result");
    } else {
        const binary = locateBrowser(options.browser);
        if (binary === null) {
            throw new Error(`no ${options.browser} on this machine`);
        }
        const profile = mkdtempSync(join(tmpdir(), "vpod-measure-"));
        child = spawn(binary, browserArguments(options.browser, pageUrl, profile), {
            stdio: "ignore",
        });
    }

    const timeout = setTimeout(() => {
        resolveResult({ failed: true, error: `timed out after ${options.timeoutSeconds}s`, rows: [] });
    }, options.timeoutSeconds * 1000);

    const report = await pendingResult;
    clearTimeout(timeout);
    child?.kill();
    await running.close();

    for (const line of report.lines ?? []) {
        console.log(line);
    }

    if (report.failed) {
        console.log(`\nFAILED: ${report.error}`);
    }

    // The native half. The page could not run it, so the table is only complete
    // once we do it here, on the same machine, right after.
    console.log("\n── native, measured here ────────────────────────────────────────");
    const tmp = mkdtempSync(join(tmpdir(), "vpod-native-"));
    const byLabel = new Map(workload().map((step) => [step.label, step]));

    const rows = (report.rows ?? []).map((row) => {
        const step = byLabel.get(row.label);
        if (step?.native === undefined) {
            return row;
        }
        if (step.group === "ripgrep") {
            spawnSync("python3", ["-c", HOST_CORPUS_SCRIPT(`${tmp}/corpus`)]);
        }
        const seconds = nativeSeconds(step.native(tmp), tmp, step.repeats ?? 1);
        console.log(`    ${row.label.padEnd(30)} ${seconds ? `${seconds.toFixed(3)}s` : "unavailable"}`);
        return { ...row, nativeSeconds: seconds };
    });

    console.log(summarize(rows));

    if (report.skipped?.length) {
        console.log(`\nskipped in the browser: ${report.skipped.join(", ")}`);
        console.log("pass --apk <mirror> to include them; see docs/browser-phases/phase-5-alpine-mirror.md");
    }

    const outputPath = join(resultsDir, `measure-${Date.now()}.json`);
    writeFileSync(outputPath, `${JSON.stringify({ ...report, rows }, null, 2)}\n`);
    console.log(`\nwrote ${outputPath}`);

    process.exit(report.failed ? 1 : 0);
}

await main();
