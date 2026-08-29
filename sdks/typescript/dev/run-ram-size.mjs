#!/usr/bin/env node

import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { browserArguments, locateBrowser } from "./browsers.mjs";
import { startServer } from "./serve.mjs";

const SMALLEST_PLAUSIBLE_SHARE = 0.85;

const options = { browser: "chrome", isolate: false, port: 8794, timeoutSeconds: 600, pairs: [] };
for (let index = 2; index < process.argv.length; index++) {
    const flag = process.argv[index];
    if (flag === "--browser") options.browser = process.argv[++index];
    else if (flag === "--isolate") options.isolate = true;
    else if (flag === "--port") options.port = Number(process.argv[++index]);
    else if (flag === "--timeout") options.timeoutSeconds = Number(process.argv[++index]);
    else if (flag === "--snapshot-dir") options.snapshotDir = process.argv[++index];
    else if (flag === "--pair") {
        const [sized, anonymous] = process.argv[++index].split("=");
        if (!sized || !anonymous) {
            console.error("--pair wants <sized-id>=<anonymous-id>");
            process.exit(1);
        }
        options.pairs.push({ sized, anonymous });
    }
}

if (options.pairs.length === 0) {
    console.error("nothing to check: pass at least one --pair <sized-id>=<anonymous-id>");
    process.exit(1);
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
    home: "/dev/ram-size.html",
    onResult: (body) => resolveReport(JSON.parse(body)),
});

const ids = options.pairs.flatMap(({ sized, anonymous }) => [sized, anonymous]);
const url =
    `http://127.0.0.1:${options.port}/dev/ram-size.html` +
    `?ids=${encodeURIComponent(ids.join(","))}`;

const profile = mkdtempSync(join(tmpdir(), "vpod-ram-size-"));
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

// The profile holds an OPFS copy of every snapshot the page pulled, so leaving
// it behind costs hundreds of megabytes per run. Wait for the browser to be
// gone first, or it is still writing into the directory being removed, and
// never let this failing hide the result.
async function discardProfile() {
    await Promise.race([once(child, "exit"), new Promise((done) => setTimeout(done, 5000))]);
    try {
        rmSync(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
    } catch (thrown) {
        console.log(`could not remove ${profile}: ${thrown.message}`);
    }
}

console.log(`\n=== ${options.browser}, isolation ${options.isolate ? "on" : "off"} ===`);
if (report.failed) {
    console.log(`FAILED: ${report.error}`);
    await discardProfile();
    process.exit(1);
}

console.log(`crossOriginIsolated  ${report.crossOriginIsolated}`);

let passed = true;
const fail = (line) => {
    passed = false;
    console.log(`FAIL ${line}`);
};

for (const { sized, anonymous } of options.pairs) {
    const named = report.totals[sized];
    const unnamed = report.totals[anonymous];
    const captured = Number(/(\d+)mb/i.exec(sized)?.[1]);

    if (named === undefined || unnamed === undefined) {
        fail(`${sized}: the page did not report both ids`);
        continue;
    }

    if (unnamed !== named) {
        fail(
            `${sized}: ${named} MB under its own name but ${unnamed} MB as ${anonymous}. ` +
                `The size is coming off the file name instead of the snapshot header.`,
        );
        continue;
    }

    if (!Number.isFinite(captured)) {
        fail(`${sized}: cannot tell what size it should be from its id`);
        continue;
    }

    if (named > captured || named < captured * SMALLEST_PLAUSIBLE_SHARE) {
        fail(`${sized}: a ${captured} MB snapshot gave a guest that sees ${named} MB`);
        continue;
    }

    console.log(`ok   ${sized}  ${named} MB, same as ${anonymous}`);
}

await discardProfile();
process.exit(passed ? 0 : 1);
