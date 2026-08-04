#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import {
    copyFileSync,
    mkdirSync,
    readFileSync,
    readdirSync,
    rmSync,
    statSync,
    writeFileSync,
} from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import * as esbuild from "esbuild";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const distDir = join(packageRoot, "dist");
const componentDir = join(distDir, "component");
const nodeComponentDir = join(distDir, "component-node");
const nodeDir = join(distDir, "node");
const localWasmDir = join(packageRoot, "wasm");
const pythonSdkWasmDir = resolve(packageRoot, "..", "python", "vpod");
const cratesDir = resolve(packageRoot, "..", "..", "crates");

const TIERS = {
    aot: "vpod_wasi_lib_aot.wasm",
    base: "vpod_wasi_lib.wasm",
};

const SHIM_ENTRY_POINTS = [
    "src/shims/cli.ts",
    "src/shims/clocks.ts",
    "src/shims/filesystem.ts",
    "src/shims/io.ts",
    "src/shims/random.ts",
    "src/shims/sockets.ts",
];

const NET_ENTRY_POINTS = [
    "src/net/entry.ts",
    "src/net/availability.ts",
    "src/net/capabilities.ts",
    "src/net/fetch-backend.ts",
    "src/net/fetch-driver.ts",
    "src/net/http-codec.ts",
    "src/net/preamble.ts",
    "src/net/ring.ts",
    "src/net/synthetic-dns.ts",
];

function parseArguments(argv) {
    let tier = "aot";
    for (let index = 0; index < argv.length; index++) {
        if (argv[index] === "--tier") {
            tier = argv[++index];
        }
    }
    if (!(tier in TIERS)) {
        throw new Error(`unknown tier '${tier}', expected one of ${Object.keys(TIERS).join(", ")}`);
    }
    return { tier };
}

function exists(path) {
    try {
        statSync(path);
        return true;
    } catch {
        return false;
    }
}

const modifiedAt = (path) => statSync(path).mtimeMs;

function newestGuestSourceTime() {
    const stack = [cratesDir];
    let newest = 0;

    while (stack.length > 0) {
        const directory = stack.pop();
        for (const entry of readdirSync(directory, { withFileTypes: true })) {
            if (entry.name === "target" || entry.name.startsWith(".")) continue;
            const path = join(directory, entry.name);

            if (entry.isDirectory()) {
                stack.push(path);
            } else if (entry.name.endsWith(".rs") || entry.name === "Cargo.toml") {
                newest = Math.max(newest, modifiedAt(path));
            }
        }
    }
    return newest;
}

function locateComponent(tier) {
    const fileName = TIERS[tier];
    const localPath = join(localWasmDir, fileName);
    const pythonSdkPath = join(pythonSdkWasmDir, fileName);

    const haveLocal = exists(localPath);
    const havePythonSdk = exists(pythonSdkPath);

    if (!haveLocal && !havePythonSdk) {
        throw new Error(
            `no ${tier} component in ${relative(packageRoot, localWasmDir)} or ${pythonSdkWasmDir}\n` +
                `Build it first: ./scripts/build-wasm.sh (from the repository root)`,
        );
    }

    if (havePythonSdk && (!haveLocal || modifiedAt(pythonSdkPath) > modifiedAt(localPath))) {
        mkdirSync(localWasmDir, { recursive: true });
        copyFileSync(pythonSdkPath, localPath);
        console.log(`[build] refreshed ${fileName} from the Python SDK`);
    }

    return localPath;
}

function assertNotStale(tier, componentPath) {
    const sourceTime = newestGuestSourceTime();
    if (modifiedAt(componentPath) >= sourceTime) {
        return;
    }

    const staleBy = (sourceTime - modifiedAt(componentPath)) / 3_600_000;
    throw new Error(
        `the ${tier} component is older than the Rust it is built from, by ` +
            `${staleBy.toFixed(1)} hours.\n` +
            `  component: ${relative(packageRoot, componentPath)}\n` +
            `Emulator changes only surface through the guest, so this builds and ` +
            `mostly passes while quietly shipping the old emulator.\n` +
            `Rebuild: ./scripts/build-wasm.sh (from the repository root)\n` +
            `Override for a deliberate mismatch: VPOD_ALLOW_STALE_COMPONENT=1`,
    );
}

async function bundle() {
    await esbuild.build({
        entryPoints: [
            "src/index.ts",
            "src/worker/entry.ts",
            "src/transport/inline.ts",
            ...SHIM_ENTRY_POINTS,
            ...NET_ENTRY_POINTS,
        ],
        absWorkingDir: packageRoot,
        outdir: "dist",
        outbase: "src",
        bundle: true,
        splitting: true,
        format: "esm",
        platform: "browser",
        target: ["es2022"],
        sourcemap: true,
        logLevel: "warning",
    });
}

async function bundleNode() {
    await esbuild.build({
        entryPoints: [
            "src/node/index.ts",
            "src/node/worker-entry.ts",
            "src/node/host-resolver.ts",
        ],
        absWorkingDir: packageRoot,
        outdir: "dist/node",
        outbase: "src/node",
        bundle: true,
        format: "esm",
        platform: "node",
        target: ["node20"],
        packages: "external",
        sourcemap: true,
        logLevel: "warning",
    });
}

function transpileForNode(componentPath) {
    const result = spawnSync(
        join(packageRoot, "node_modules", ".bin", "jco"),
        [
            "transpile",
            componentPath,
            "-o",
            nodeComponentDir,
            "--name",
            "vpod",
            "--quiet",
            "--map",
            "wasi:sockets/ip-name-lookup=../node/host-resolver.js#ipNameLookup",
        ],
        { stdio: "inherit", cwd: packageRoot },
    );

    if (result.status !== 0) {
        throw new Error(`jco transpile (node) failed with status ${result.status}`);
    }
}

function transpile(componentPath) {
    // Mapping the WASI imports to relative paths
    const mappings = [
        "wasi:cli/*=../shims/cli.js#*",
        "wasi:clocks/*=../shims/clocks.js#*",
        "wasi:filesystem/*=../shims/filesystem.js#*",
        "wasi:io/*=../shims/io.js#*",
        "wasi:random/*=../shims/random.js#*",
        "wasi:sockets/*=../shims/sockets.js#*",
    ];

    const result = spawnSync(
        join(packageRoot, "node_modules", ".bin", "jco"),
        [
            "transpile",
            componentPath,
            "-o",
            componentDir,
            "--name",
            "vpod",
            "--quiet",
            ...mappings.flatMap((mapping) => ["--map", mapping]),
        ],
        { stdio: "inherit", cwd: packageRoot },
    );

    if (result.status !== 0) {
        throw new Error(`jco transpile failed with status ${result.status}`);
    }
}


function shareCoreModule() {
    const shared = join(componentDir, "vpod.core.wasm");
    const duplicate = join(nodeComponentDir, "vpod.core.wasm");

    if (!readFileSync(shared).equals(readFileSync(duplicate))) {
        console.log("[build] node core wasm differs from the browser one, keeping both");
        return;
    }

    const loader = join(nodeComponentDir, "vpod.js");
    const source = readFileSync(loader, "utf8");
    const reference = "new URL('./vpod.core.wasm', import.meta.url)";

    if (!source.includes(reference)) {
        throw new Error(`cannot redirect the node core wasm: ${reference} not found in vpod.js`);
    }

    writeFileSync(
        loader,
        source.replace(reference, "new URL('../component/vpod.core.wasm', import.meta.url)"),
    );
    rmSync(duplicate);
}

function declarations() {
    const result = spawnSync(
        join(packageRoot, "node_modules", ".bin", "tsc"),
        ["-p", "tsconfig.build.json"],
        { cwd: packageRoot, stdio: "inherit" },
    );

    if (result.status !== 0) {
        throw new Error("tsc failed to emit declarations");
    }

    for (const entry of [join(distDir, "index.d.ts"), join(nodeDir, "index.d.ts")]) {
        writeFileSync(entry, `/// <reference lib="esnext.disposable" />\n${readFileSync(entry)}`);
    }
}

async function main() {
    const { tier } = parseArguments(process.argv.slice(2));
    const componentPath = locateComponent(tier);

    if (process.env.VPOD_ALLOW_STALE_COMPONENT !== "1") {
        assertNotStale(tier, componentPath);
    }

    rmSync(distDir, { recursive: true, force: true });
    mkdirSync(componentDir, { recursive: true });

    console.log(`[build] tier: ${tier} (${relative(packageRoot, componentPath)})`);
    await bundle();
    transpile(componentPath);

    mkdirSync(nodeComponentDir, { recursive: true });
    transpileForNode(componentPath);
    await bundleNode();
    shareCoreModule();

    const manifest = {
        tier,
        source: relative(packageRoot, componentPath),
        componentBytes: statSync(componentPath).size,
        coreWasmBytes: statSync(join(componentDir, "vpod.core.wasm")).size,
        builtAt: new Date().toISOString(),
    };
    writeFileSync(
        join(componentDir, "manifest.json"),
        `${JSON.stringify(manifest, null, 2)}\n`,
    );

    declarations();

    const megabytes = (bytes) => `${(bytes / 1048576).toFixed(1)} MiB`;
    console.log(`[build] core wasm: ${megabytes(manifest.coreWasmBytes)}`);
    console.log(`[build] done -> ${relative(packageRoot, distDir)}`);
}

await main();
