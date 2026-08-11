#!/usr/bin/env node

import { spawn } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { startServer } from "./serve.mjs";

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

const options = { browser: "chrome", isolate: false, port: 8793, timeoutSeconds: 300 };
for (let index = 2; index < process.argv.length; index++) {
    const flag = process.argv[index];
    if (flag === "--browser") options.browser = process.argv[++index];
    else if (flag === "--isolate") options.isolate = true;
    else if (flag === "--port") options.port = Number(process.argv[++index]);
    else if (flag === "--timeout") options.timeoutSeconds = Number(process.argv[++index]);
}

const browser = BROWSERS[options.browser];
if (browser === undefined) {
    console.error(`unknown browser: ${options.browser}`);
    process.exit(1);
}

let resolveReport;
const reported = new Promise((resolve) => {
    resolveReport = resolve;
});

const server = await startServer({
    port: options.port,
    isolate: options.isolate,
    home: "/dev/interrupt.html",
    onResult: (body) => resolveReport(JSON.parse(body)),
});

const profile = mkdtempSync(join(tmpdir(), "vpod-interrupt-"));
const url = `http://127.0.0.1:${options.port}/dev/interrupt.html`;
const child = spawn(browser.binary, browser.args(url, profile), { stdio: "ignore" });

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
console.log(`round trip           ${report.perCallMilliseconds?.toFixed(2)} ms per short command`);
console.log(`long command         ${report.longCommandSeconds?.toFixed(2)}s`);
console.log(
    `main thread          ${report.frames} frames, worst gap ${report.worstGapMilliseconds?.toFixed(0)}ms`,
);

process.exit(report.passed ? 0 : 1);
