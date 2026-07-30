#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import * as esbuild from "esbuild";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const distDir = join(packageRoot, "dist");
const componentDir = join(distDir, "component");
const localWasmDir = join(packageRoot, "wasm");
const pythonSdkWasmDir = resolve(packageRoot, "..", "python", "vpod");

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

function locateComponent(tier) {
    const fileName = TIERS[tier];
    const localPath = join(localWasmDir, fileName);

    if (exists(localPath)) {
        return localPath;
    }

    const pythonSdkPath = join(pythonSdkWasmDir, fileName);
    if (exists(pythonSdkPath)) {
        mkdirSync(localWasmDir, { recursive: true });
        copyFileSync(pythonSdkPath, localPath);
        console.log(`[build] copied ${fileName} into wasm/`);
        return localPath;
    }

    throw new Error(
        `no ${tier} component in ${relative(packageRoot, localWasmDir)} or ${pythonSdkWasmDir}\n` +
            `Build it first: ./scripts/build-wasm.sh (from the repository root)`,
    );
}

async function bundle() {
    await esbuild.build({
        entryPoints: ["src/index.ts", "src/worker/entry.ts", "src/transport/inline.ts", ...SHIM_ENTRY_POINTS],
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

async function main() {
    const { tier } = parseArguments(process.argv.slice(2));
    const componentPath = locateComponent(tier);

    rmSync(distDir, { recursive: true, force: true });
    mkdirSync(componentDir, { recursive: true });

    console.log(`[build] tier: ${tier} (${relative(packageRoot, componentPath)})`);
    await bundle();
    transpile(componentPath);

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

    const megabytes = (bytes) => `${(bytes / 1048576).toFixed(1)} MiB`;
    console.log(`[build] core wasm: ${megabytes(manifest.coreWasmBytes)}`);
    console.log(`[build] done -> ${relative(packageRoot, distDir)}`);
}

await main();
