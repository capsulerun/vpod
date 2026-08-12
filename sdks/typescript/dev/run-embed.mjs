#!/usr/bin/env node

import { spawn } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { browserArguments, locateBrowser } from "./browsers.mjs";
import { startServer } from "./serve.mjs";


const options = { browser: "chrome", isolate: false, port: 8794, timeoutSeconds: 300 };
for (let index = 2; index < process.argv.length; index++) {
    const flag = process.argv[index];
    if (flag === "--browser") options.browser = process.argv[++index];
    else if (flag === "--isolate") options.isolate = true;
    else if (flag === "--port") options.port = Number(process.argv[++index]);
    else if (flag === "--name") options.name = process.argv[++index];
    else if (flag === "--snapshot-dir") options.snapshotDir = process.argv[++index];
    else if (flag === "--timeout") options.timeoutSeconds = Number(process.argv[++index]);
}

const binary = locateBrowser(options.browser);
if (binary === null) {
    console.error(`no ${options.browser} on this machine`);
    process.exit(1);
}

let resolveReport;
const reported = new Promise((resolve) => {
    resolveReport = resolve;
});

const server = await startServer({
    port: options.port,
    isolate: options.isolate,
    snapshotDir: options.snapshotDir,
    home: "/dev/embed.html",
    onResult: (body) => resolveReport(JSON.parse(body)),
});

const profile = mkdtempSync(join(tmpdir(), "vpod-embed-"));
const query = options.name ? `?name=${encodeURIComponent(options.name)}` : "";
const url = `http://127.0.0.1:${options.port}/dev/embed.html${query}`;
const child = spawn(binary, browserArguments(options.browser, url, profile), {
    stdio: "ignore",
});

const timeout = setTimeout(() => {
    resolveReport({ failed: true, error: `no result within ${options.timeoutSeconds}s` });
}, options.timeoutSeconds * 1000);

const report = await reported;
clearTimeout(timeout);
child.kill("SIGKILL");
server.close();

console.log(`\n=== ${options.browser}, isolation ${options.isolate ? "on" : "off"} ===`);
if (report.failed) {
    console.log(`FAILED: ${report.error}`);
    process.exit(1);
}

console.log(`crossOriginIsolated  ${report.crossOriginIsolated}`);
for (const entry of report.checks) {
    console.log(`${entry.passed ? "ok  " : "FAIL"} ${entry.name}  ${entry.detail ?? ""}`);
}

process.exit(report.passed ? 0 : 1);
