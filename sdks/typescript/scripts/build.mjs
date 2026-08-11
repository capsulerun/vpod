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
const embedDir = join(distDir, "embed");
const embedComponentDir = join(distDir, "component-embed");
const localWasmDir = join(packageRoot, "wasm");
const pythonSdkWasmDir = resolve(packageRoot, "..", "python", "vpod");
const cratesDir = resolve(packageRoot, "..", "..", "crates");

const TIERS = {
    aot: "vpod_wasi_lib_aot.wasm",
    base: "vpod_wasi_lib.wasm",
};

const INSTANTIATION_ARGUMENTS = ["-I", "async"];

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

/**
 * The embed artifact: two files with no imports between them, each loadable straight from
 * `URL.createObjectURL(new Blob([source]))`. A blob: URL has no path, so a relative
 * specifier cannot resolve against it, which is why splitting is off and why the worker
 * has the component glue bundled in rather than importing it at runtime.
 *
 * The core wasm is not bundled. The embedder passes it as bytes through `coreModules`,
 * which is the whole point: 25 MiB of base64 would inflate by a third and defeat caching.
 */
async function bundleEmbed(componentPath) {
    mkdirSync(embedDir, { recursive: true });

    // The shipped glue keeps a `node:fs` fallback for reading core wasm off disk under
    // Node, which the default build needs and the embed build must not have: esbuild
    // refuses to resolve it for the browser, and leaving it external would put a Node
    // builtin in a browser bundle. The embed worker always gets its bytes through
    // `coreModules`, so transpile a second glue without it. Only the JS is used; the
    // core wasm it emits alongside is identical to the shipped one and thrown away.
    transpile(componentPath, embedComponentDir, ["--no-nodejs-compat"]);
    const gluePath = join(embedComponentDir, "vpod.js");

    const shared = {
        absWorkingDir: packageRoot,
        bundle: true,
        splitting: false,
        format: "esm",
        platform: "browser",
        target: ["es2022"],
        logLevel: "warning",
    };

    await esbuild.build({
        ...shared,
        entryPoints: ["src/index.ts"],
        outfile: join(embedDir, "vpod.js"),
    });

    await esbuild.build({
        ...shared,
        stdin: {
            contents:
                `import * as component from ${JSON.stringify(gluePath)};\n` +
                `import { serveWorker } from "./src/worker/serve.ts";\n` +
                `serveWorker(component);\n`,
            resolveDir: packageRoot,
            loader: "ts",
        },
        outfile: join(embedDir, "vpod.worker.js"),
    });

    rmSync(embedComponentDir, { recursive: true, force: true });

    for (const name of ["vpod.js", "vpod.worker.js"]) {
        assertLoadableFromBlob(join(embedDir, name));
    }
}

/**
 * Anchored on the quoted specifier rather than on the `import` keyword, because esbuild
 * writes multi-line import statements and a line-anchored pattern silently matches none
 * of them, which is a guard that always passes.
 */
const RELATIVE_SPECIFIER_PATTERNS = [
    /\bfrom\s*["']([^"']+)["']/g,
    /\bimport\s+["']([^"']+)["']/g,
    /\bimport\s*\(\s*["']([^"']+)["']/g,
];

export function relativeImportsIn(source) {
    const found = new Set();
    for (const pattern of RELATIVE_SPECIFIER_PATTERNS) {
        for (const match of source.matchAll(pattern)) {
            if (match[1].startsWith(".")) {
                found.add(match[1]);
            }
        }
    }
    return [...found];
}

/**
 * The failure this guards against is silent: a relative specifier in a blob-loaded module
 * fails with an opaque module resolution error and no clue which import caused it.
 */
function assertLoadableFromBlob(filePath) {
    const specifiers = relativeImportsIn(readFileSync(filePath, "utf8"));

    if (specifiers.length > 0) {
        throw new Error(
            `${relative(packageRoot, filePath)} keeps relative imports and cannot load from a ` +
                `blob: URL: ${specifiers.join(", ")}`,
        );
    }
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
            ...INSTANTIATION_ARGUMENTS,
            "--map",
            "wasi:sockets/ip-name-lookup=../node/host-resolver.js#ipNameLookup",
        ],
        { stdio: "inherit", cwd: packageRoot },
    );

    if (result.status !== 0) {
        throw new Error(`jco transpile (node) failed with status ${result.status}`);
    }
}

// Mapping the WASI imports to relative paths
const BROWSER_MAPPINGS = [
    "wasi:cli/*=../shims/cli.js#*",
    "wasi:clocks/*=../shims/clocks.js#*",
    "wasi:filesystem/*=../shims/filesystem.js#*",
    "wasi:io/*=../shims/io.js#*",
    "wasi:random/*=../shims/random.js#*",
    "wasi:sockets/*=../shims/sockets.js#*",
];

function transpile(componentPath, outputDir = componentDir, extraArguments = []) {
    const result = spawnSync(
        join(packageRoot, "node_modules", ".bin", "jco"),
        [
            "transpile",
            componentPath,
            "-o",
            outputDir,
            "--name",
            "vpod",
            "--quiet",
            ...INSTANTIATION_ARGUMENTS,
            ...extraArguments,
            ...BROWSER_MAPPINGS.flatMap((mapping) => ["--map", mapping]),
        ],
        { stdio: "inherit", cwd: packageRoot },
    );

    if (result.status !== 0) {
        throw new Error(`jco transpile failed with status ${result.status}`);
    }
}


const coreModulesIn = (directory) =>
    readdirSync(directory)
        .filter((name) => name.endsWith(".wasm"))
        .sort();

/**
 * Both components are transpiled from the same wasm, so their core modules come out
 * byte-identical and only one copy needs shipping. This used to rewrite a URL inside the
 * generated loader; under custom instantiation there is no URL to rewrite, because
 * `loadCoreModule` in src/node/component-imports.ts looks beside the component and then
 * in the browser component's directory.
 */
function shareCoreModules() {
    const browserModules = coreModulesIn(componentDir);
    const nodeModules = coreModulesIn(nodeComponentDir);

    const identical =
        browserModules.join() === nodeModules.join() &&
        browserModules.every((name) =>
            readFileSync(join(componentDir, name)).equals(
                readFileSync(join(nodeComponentDir, name)),
            ),
        );

    if (!identical) {
        console.log("[build] node core wasm differs from the browser one, keeping both");
        return;
    }

    for (const name of nodeModules) {
        rmSync(join(nodeComponentDir, name));
    }
    console.log(
        `[build] node shares ${browserModules.length} core module(s) with the browser component`,
    );
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
    shareCoreModules();

    // After transpile: the worker bundles the glue the transpile step emits.
    await bundleEmbed(componentPath);

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
    console.log(
        `[build] embed: ${["vpod.js", "vpod.worker.js"]
            .map((name) => `${name} ${megabytes(statSync(join(embedDir, name)).size)}`)
            .join(", ")}`,
    );
    console.log(`[build] done -> ${relative(packageRoot, distDir)}`);
}

// Importable for its checks without running a build, the way dev/serve.mjs is.
if (process.argv[1] === fileURLToPath(import.meta.url)) {
    await main();
}
