#!/usr/bin/env node

import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repositoryRoot = resolve(packageRoot, "..", "..");

function locateSnapshot() {
    return [
        process.env.VPOD_TEST_SNAPSHOT,
        join(homedir(), "Library", "Application Support", "vpod", "snapshots", "alpine-3.23.0-256mb.snap"),
        join(repositoryRoot, "dist", "alpine-3.23.0-256mb.snap"),
    ].find((candidate) => candidate && existsSync(candidate));
}

const snapshotPath = locateSnapshot();
if (snapshotPath === undefined) {
    console.error("no snapshot found; set VPOD_TEST_SNAPSHOT");
    process.exit(1);
}

const step = async (name, run) => {
    const startedAt = performance.now();
    try {
        const detail = await run();
        const note = typeof detail === "string" ? detail : "";
        console.log(`ok   ${name} (${((performance.now() - startedAt) / 1000).toFixed(2)}s) ${note}`);
        return detail;
    } catch (thrown) {
        console.log(`FAIL ${name} (${((performance.now() - startedAt) / 1000).toFixed(2)}s)`);
        console.log(String(thrown?.stack ?? thrown).split("\n").slice(0, 6).join("\n"));
        throw thrown;
    }
};

const filesystem = await import("@bytecodealliance/preview2-shim/filesystem");
if (typeof filesystem._setPreopens === "function") {
    filesystem._setPreopens({ "/": "/" });
}

const { executor } = await step("load the node component", async () => {
    return await import(new URL("../dist/component-node/vpod.js", import.meta.url).href);
});

const handle = await step("session-start", async () =>
    executor.sessionStart(snapshotPath, "/bin/sh", "# ", []),
);

const run = (code, seconds = 60) => executor.sessionExec(handle, code, BigInt(seconds));

await step("guest runs at all", async () => {
    const result = run("echo alive");
    return `exit=${result.exitCode} ${JSON.stringify(result.stdout.trim())}`;
});

await step("dns resolves for real", async () => {
    const result = run("getent hosts pypi.org || nslookup pypi.org");
    return `exit=${result.exitCode} ${JSON.stringify(result.stdout.trim().slice(0, 100))}`;
});

await step("https through real sockets, no CORS", async () => {
    const result = run("wget -q -O- https://pypi.org/pypi/six/json | head -c 60");
    return `exit=${result.exitCode} ${JSON.stringify(result.stdout.trim().slice(0, 60))}`;
});

await step("apk, the thing a browser cannot reach at all", async () => {
    const result = run("apk update 2>&1 | tail -2", 180);
    return `exit=${result.exitCode} ${JSON.stringify(result.stdout.trim().slice(-120))}`;
});

await step("uv installs a pure wheel", async () => {
    const result = run("uv pip install --system six 2>&1 | tail -2", 240);
    return `exit=${result.exitCode} ${JSON.stringify(result.stdout.trim().slice(-100))}`;
});

await step("the installed package imports", async () => {
    const result = run("python3 -c 'import six; print(six.__version__)'", 60);
    return `exit=${result.exitCode} ${JSON.stringify(result.stdout.trim())}`;
});

await step("apk installs a package", async () => {
    const result = run("apk add --no-cache jq 2>&1 | tail -2 && jq --version", 240);
    return `exit=${result.exitCode} ${JSON.stringify(result.stdout.trim().slice(-80))}`;
});

executor.sessionClose(handle);
console.log("\ndone");
