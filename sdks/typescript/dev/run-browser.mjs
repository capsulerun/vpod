#!/usr/bin/env node

import { spawn } from "node:child_process";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { startServer } from "./serve.mjs";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const resultsDir = join(packageRoot, "dev", "results");

const BROWSERS = {
    chrome: {
        binary: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        args: (url, profile) => [
            "--headless=new",
            "--disable-gpu",
            "--no-first-run",
            "--no-default-browser-check",
            `--user-data-dir=${profile}`,
            url,
        ],
    },
    firefox: {
        binary: "/Applications/Firefox.app/Contents/MacOS/firefox",
        args: (url, profile) => ["--headless", "--profile", profile, url],
    },
};

function processTree(rootPid) {
    let listing;
    try {
        listing = execFileSync("ps", ["-eo", "pid=,ppid=,rss="], { encoding: "utf8" });
    } catch {
        return [];
    }

    const children = new Map();
    const residentKilobytes = new Map();
    for (const line of listing.trim().split("\n")) {
        const [pid, parentPid, rss] = line.trim().split(/\s+/).map(Number);
        if (!children.has(parentPid)) children.set(parentPid, []);
        children.get(parentPid).push(pid);
        residentKilobytes.set(pid, rss);
    }

    const tree = [];
    const queue = [rootPid];
    while (queue.length > 0) {
        const pid = queue.pop();
        if (residentKilobytes.has(pid)) {
            tree.push({ pid, residentBytes: residentKilobytes.get(pid) * 1024 });
        }
        queue.push(...(children.get(pid) ?? []));
    }
    return tree;
}

function sampleMemory(rootPid, state) {
    const tree = processTree(rootPid);
    let total = 0;
    let largest = 0;
    for (const { residentBytes } of tree) {
        total += residentBytes;
        if (residentBytes > largest) largest = residentBytes;
    }
    if (total > state.peakTreeBytes) state.peakTreeBytes = total;
    if (largest > state.peakProcessBytes) state.peakProcessBytes = largest;
    state.samples++;
}

function sampleMatchingProcesses(pattern, state) {
    let listing;
    try {
        listing = execFileSync("ps", ["-eo", "rss=,command="], { encoding: "utf8" });
    } catch {
        return;
    }

    let total = 0;
    let largest = 0;
    for (const line of listing.split("\n")) {
        if (!line.includes(pattern)) continue;
        const residentBytes = Number(line.trim().split(/\s+/)[0]) * 1024;
        if (!Number.isFinite(residentBytes)) continue;
        total += residentBytes;
        if (residentBytes > largest) largest = residentBytes;
    }
    if (total > state.peakTreeBytes) state.peakTreeBytes = total;
    if (largest > state.peakProcessBytes) state.peakProcessBytes = largest;
    state.samples++;
}

function parseArguments(argv) {
    const options = {
        browser: "chrome",
        visits: 1,
        isolate: false,
        attach: false,
        port: 8791,
        timeoutSeconds: 180,
    };
    for (let index = 0; index < argv.length; index++) {
        const flag = argv[index];
        if (flag === "--browser") options.browser = argv[++index];
        else if (flag === "--visits") options.visits = Number(argv[++index]);
        else if (flag === "--isolate") options.isolate = true;
        else if (flag === "--attach") options.attach = true;
        else if (flag === "--port") options.port = Number(argv[++index]);
        else if (flag === "--name") options.name = argv[++index];
        else if (flag === "--match") options.match = argv[++index];
        else if (flag === "--snapshot-dir") options.snapshotDir = argv[++index];
        else if (flag === "--timeout") options.timeoutSeconds = Number(argv[++index]);
    }
    return options;
}

function summarize(report) {
    if (report.failed) {
        return `FAILED: ${report.error}`;
    }
    const seconds = (milliseconds) => `${(milliseconds / 1000).toFixed(3)}s`;
    return [
        `tier              ${report.tier}`,
        `isolated          ${report.crossOriginIsolated}`,
        `compile+instantiate ${seconds(report.componentLoadMilliseconds)}`,
        `pull ${report.snapshotId} ${seconds(report.fetchMilliseconds)} from ${report.snapshotSource} (${(report.snapshotBytes / 1048576).toFixed(0)} MiB: acquire ${seconds(report.pullAcquireMilliseconds)}, verify ${seconds(report.pullVerifyMilliseconds)}, store ${seconds(report.pullStoreMilliseconds)})`,
        `session-start     ${seconds(report.startMilliseconds)}`,
        `2nd session-start ${seconds(report.secondStartMilliseconds)} (base cache)`,
        `first exec        ${seconds(report.warmMilliseconds)}`,
        `ab_loop median    ${report.abLoopMedianSeconds.toFixed(3)}s`,
        `output correct    ${report.outputCorrect}`,
        `poll spins        ${report.pollSpinCount} totalling ${(report.pollSpinNanoseconds / 1e9).toFixed(3)}s`,
        `main thread       ${report.mainThread.frames} frames, worst gap ${report.mainThread.worstGapMilliseconds.toFixed(0)}ms`,
    ].join("\n  ");
}

async function main() {
    const options = parseArguments(process.argv.slice(2));
    mkdirSync(resultsDir, { recursive: true });

    let resolveResult;
    let pendingResult = new Promise((done) => {
        resolveResult = done;
    });

    const running = await startServer({
        port: options.port,
        isolate: options.isolate,
        snapshotDir: options.snapshotDir,
        onResult: (body) => resolveResult(JSON.parse(body)),
    });

    const pageUrl = options.name
        ? `${running.url}?name=${encodeURIComponent(options.name)}`
        : running.url;

    console.log(`serving ${pageUrl} (snapshots from ${running.snapshots})`);

    if (options.attach) {
        console.log(`open this and wait: ${pageUrl}`);

        const pattern = options.match ?? "WebContent";
        const memoryState = { peakTreeBytes: 0, peakProcessBytes: 0, samples: 0 };
        const sampler = setInterval(() => {
            sampleMatchingProcesses(pattern, memoryState);
        }, 250);

        const timeout = setTimeout(() => {
            resolveResult({ failed: true, error: `timed out after ${options.timeoutSeconds}s` });
        }, options.timeoutSeconds * 1000);

        const report = await pendingResult;
        clearInterval(sampler);
        clearTimeout(timeout);

        report.browser = options.browser === "chrome" ? "attached" : options.browser;
        report.peakProcessBytes = memoryState.peakProcessBytes;
        report.peakTreeBytes = memoryState.peakTreeBytes;
        report.memoryMatch = pattern;

        const megabytes = (bytes) => `${(bytes / 1048576).toFixed(0)} MiB`;
        console.log(`\n  ${summarize(report)}`);
        console.log(
            `  peak memory       ${megabytes(memoryState.peakProcessBytes)} largest '${pattern}' process, ${megabytes(memoryState.peakTreeBytes)} all matching`,
        );

        const outputPath = join(resultsDir, `attached-${Date.now()}.json`);
        writeFileSync(outputPath, `${JSON.stringify([report], null, 2)}\n`);
        console.log(`\nwrote ${outputPath}`);
        await running.close();
        return;
    }

    const browser = BROWSERS[options.browser];
    if (browser === undefined) {
        throw new Error(`unknown browser '${options.browser}'`);
    }

    const profile = mkdtempSync(join(tmpdir(), `vpod-${options.browser}-`));
    const reports = [];

    for (let visit = 0; visit < options.visits; visit++) {
        pendingResult = new Promise((done) => {
            resolveResult = done;
        });

        const child = spawn(browser.binary, browser.args(pageUrl, profile), {
            stdio: "ignore",
        });

        const memoryState = { peakTreeBytes: 0, peakProcessBytes: 0, samples: 0 };
        const sampler = setInterval(() => sampleMemory(child.pid, memoryState), 250);

        const timeout = setTimeout(() => {
            resolveResult({ failed: true, error: `timed out after ${options.timeoutSeconds}s` });
        }, options.timeoutSeconds * 1000);

        const report = await pendingResult;
        clearInterval(sampler);
        clearTimeout(timeout);
        child.kill("SIGKILL");

        report.visit = visit;
        report.browser = options.browser;
        report.peakTreeBytes = memoryState.peakTreeBytes;
        report.peakProcessBytes = memoryState.peakProcessBytes;
        report.memorySamples = memoryState.samples;
        reports.push(report);

        const megabytes = (bytes) => `${(bytes / 1048576).toFixed(0)} MiB`;
        console.log(`\nvisit ${visit} (${visit === 0 ? "cold" : "warm"} profile)`);
        console.log(`  ${summarize(report)}`);
        console.log(`  peak memory       ${megabytes(memoryState.peakProcessBytes)} largest process, ${megabytes(memoryState.peakTreeBytes)} whole tree`);

        await new Promise((done) => setTimeout(done, 1500));
    }

    if (reports.length > 1 && !reports.some((report) => report.failed)) {
        const cold = reports[0].componentLoadMilliseconds;
        const warm = reports[reports.length - 1].componentLoadMilliseconds;
        console.log(
            `\ncode cache: compile+instantiate ${(cold / 1000).toFixed(3)}s cold -> ${(warm / 1000).toFixed(3)}s warm (${(cold / warm).toFixed(2)}x)`,
        );
    }

    const outputPath = join(resultsDir, `${options.browser}-${Date.now()}.json`);
    writeFileSync(outputPath, `${JSON.stringify(reports, null, 2)}\n`);
    console.log(`\nwrote ${outputPath}`);

    await running.close();
}

await main();
