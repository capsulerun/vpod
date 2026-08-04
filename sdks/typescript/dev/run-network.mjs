#!/usr/bin/env node

import { spawn } from "node:child_process";
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

function parseArguments(argv) {
    const options = { browser: "chrome", port: 8793, timeoutSeconds: 420, attach: false };
    for (let index = 0; index < argv.length; index++) {
        const flag = argv[index];
        if (flag === "--browser") options.browser = argv[++index];
        else if (flag === "--port") options.port = Number(argv[++index]);
        else if (flag === "--timeout") options.timeoutSeconds = Number(argv[++index]);
        else if (flag === "--attach") options.attach = true;
        else if (flag === "--name") options.name = argv[++index];
        else if (flag === "--apk") options.apk = argv[++index];
        else if (flag === "--gaps") options.gaps = true;
        else if (flag === "--snapshot-dir") options.snapshotDir = argv[++index];
    }
    return options;
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
    if (options.name) parameters.set("name", options.name);
    if (options.apk) parameters.set("apk", options.apk);
    if (options.gaps) parameters.set("gaps", "1");

    const query = parameters.size > 0 ? `?${parameters}` : "";
    const pageUrl = `${running.url}dev/network.html${query}`;
    console.log(`serving ${pageUrl} (snapshots from ${running.snapshots}, COOP/COEP on)`);

    let child;
    if (options.attach) {
        console.log("open the URL above; waiting for its result");
    } else {
        const browser = BROWSERS[options.browser];
        if (browser === undefined) {
            throw new Error(`unknown browser '${options.browser}'`);
        }
        const profile = mkdtempSync(join(tmpdir(), "vpod-net-"));
        child = spawn(browser.binary, browser.args(pageUrl, profile), { stdio: "ignore" });
    }

    const timeout = setTimeout(() => {
        resolveResult({ failed: true, error: `timed out after ${options.timeoutSeconds}s`, steps: [] });
    }, options.timeoutSeconds * 1000);

    const report = await pendingResult;
    clearTimeout(timeout);
    child?.kill();
    await running.close();

    console.log("");
    for (const entry of report.steps ?? []) {
        const mark = entry.ok ? "ok  " : "FAIL";
        console.log(`  ${mark} ${entry.name.padEnd(34)} ${(entry.milliseconds / 1000).toFixed(2)}s  ${String(entry.detail ?? "").split("\n")[0]}`);
    }
    if (report.failed) {
        console.log(`\n  FAILED: ${report.error}`);
    }

    const outputPath = join(resultsDir, `network-${Date.now()}.json`);
    writeFileSync(outputPath, `${JSON.stringify(report, null, 2)}\n`);
    console.log(`\nwrote ${outputPath}`);

    process.exit(report.failed ? 1 : 0);
}

await main();
