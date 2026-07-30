#!/usr/bin/env node

import { createHash } from "node:crypto";
import { createReadStream, existsSync, readdirSync, statSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { dirname, extname, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repositoryRoot = resolve(packageRoot, "..", "..");

const CONTENT_TYPES = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".mjs": "text/javascript; charset=utf-8",
    ".json": "application/json; charset=utf-8",
    ".map": "application/json; charset=utf-8",
    ".wasm": "application/wasm",
    ".css": "text/css; charset=utf-8",
};

const SNAPSHOT_SEARCH_PATH = [
    join(
        process.env.HOME ?? "",
        "Library",
        "Application Support",
        "vpod",
        "snapshots",
    ),
    join(process.env.HOME ?? "", ".local", "share", "vpod", "snapshots"),
    join(repositoryRoot, "dist"),
];

function findSnapshotDirectory(override) {
    if (override) {
        return resolve(override);
    }
    for (const candidate of SNAPSHOT_SEARCH_PATH) {
        if (existsSync(candidate)) {
            return candidate;
        }
    }
    return SNAPSHOT_SEARCH_PATH[0];
}

export function startServer({
    port = 8791,
    isolate = false,
    snapshotDir,
    onResult,
} = {}) {
    const snapshots = findSnapshotDirectory(snapshotDir);

    const server = createServer((request, response) => {
        const url = new URL(request.url, `http://${request.headers.host}`);
        const path = decodeURIComponent(url.pathname);

        if (isolate) {
            response.setHeader("Cross-Origin-Opener-Policy", "same-origin");
            response.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
        }
        response.setHeader("Cross-Origin-Resource-Policy", "same-origin");

        if (request.method === "POST" && path === "/result") {
            let body = "";
            request.on("data", (chunk) => {
                body += chunk;
            });
            request.on("end", () => {
                response.writeHead(204).end();
                onResult?.(body);
            });
            return;
        }

        if (path === "/registry/snapshots.json") {
            buildCatalogue(snapshots)
                .then((catalogue) => {
                    response.writeHead(200, {
                        "Content-Type": "application/json; charset=utf-8",
                        "Access-Control-Allow-Origin": "*",
                        "Cache-Control": "no-store",
                    });
                    response.end(JSON.stringify(catalogue, null, 2));
                })
                .catch((error) => {
                    response.writeHead(500).end(String(error));
                });
            return;
        }

        if (path.startsWith("/snapshot/")) {
            serveSnapshot(response, snapshots, path.slice("/snapshot/".length));
            return;
        }

        if (path === "/build-id") {
            response.writeHead(200, {
                "Content-Type": "application/json; charset=utf-8",
                "Cache-Control": "no-store",
            });
            response.end(JSON.stringify({ buildId: currentBuildId() }));
            return;
        }

        const versioned = path.match(/^\/b\/[^/]+(\/.*)$/);
        const relativePath = versioned ? versioned[1] : path === "/" ? "/dev/index.html" : path;
        const filePath = join(packageRoot, normalize(relativePath).replace(/^(\.\.[/\\])+/, ""));

        if (!filePath.startsWith(packageRoot) || !existsSync(filePath)) {
            console.error(`404 ${path}`);
            response.writeHead(404).end(`not found: ${path}\n`);
            return;
        }

        const headers = {
            "Content-Type": CONTENT_TYPES[extname(filePath)] ?? "application/octet-stream",
            "Content-Length": statSync(filePath).size,
        };

        headers["Cache-Control"] =
            versioned && relativePath.startsWith("/dist/")
                ? "public, max-age=31536000, immutable"
                : "no-store";

        response.writeHead(200, headers);
        createReadStream(filePath).pipe(response);
    });

    return new Promise((resolveServer) => {
        server.listen(port, "127.0.0.1", () => {
            resolveServer({
                server,
                port,
                snapshots,
                url: `http://127.0.0.1:${port}/`,
                close: () => new Promise((done) => server.close(done)),
            });
        });
    });
}

function currentBuildId() {
    try {
        return statSync(join(packageRoot, "dist", "index.js")).mtimeMs.toString(36);
    } catch {
        return "unbuilt";
    }
}

const catalogueCache = new Map();

function describeSnapshot(fileName) {
    const stem = fileName.replace(/\.(snap|raw)$/, "");
    const suffix = fileName.endsWith(".raw") ? "-raw" : "";
    const memory = stem.match(/-(\d+)(mb|gb)$/i);
    const withoutMemory = memory ? stem.slice(0, memory.index) : stem;
    const version = withoutMemory.match(/^(.*)-(\d+(?:\.\d+)+)$/);

    return {
        id: `${stem}${suffix}`,
        name: `${version ? version[1] : withoutMemory}${suffix}`,
        tag: version ? version[2] : "1.0.0",
        memory_label: memory ? `${memory[1]}${memory[2].toUpperCase()}` : "256MB",
    };
}

async function sha256File(filePath) {
    const hash = createHash("sha256");
    for await (const chunk of createReadStream(filePath)) {
        hash.update(chunk);
    }
    return hash.digest("hex");
}

async function buildCatalogue(snapshotDirectory) {
    const files = readdirSync(snapshotDirectory).filter((name) =>
        /\.(snap|raw)$/.test(name),
    );

    const snapshots = [];
    for (const fileName of files) {
        const filePath = join(snapshotDirectory, fileName);
        const stats = statSync(filePath);
        const key = `${filePath}:${stats.mtimeMs}:${stats.size}`;

        if (!catalogueCache.has(key)) {
            catalogueCache.set(key, await sha256File(filePath));
        }

        snapshots.push({
            ...describeSnapshot(fileName),
            description: `local ${fileName}`,
            url: `/snapshot/${fileName}`,
            sha256: catalogueCache.get(key),
            size: stats.size,
        });
    }

    return { version: "local", snapshots };
}

function serveSnapshot(response, snapshotDirectory, name) {
    const filePath = join(snapshotDirectory, normalize(name).replace(/^(\.\.[/\\])+/, ""));
    if (!filePath.startsWith(snapshotDirectory) || !existsSync(filePath)) {
        response.writeHead(404).end(`no snapshot: ${name}\n`);
        return;
    }
    response.writeHead(200, {
        "Content-Type": "application/octet-stream",
        "Content-Length": statSync(filePath).size,
        "Cache-Control": "public, max-age=31536000, immutable",
        "Access-Control-Allow-Origin": "*",
        "Accept-Ranges": "bytes",
    });
    createReadStream(filePath).pipe(response);
}

function parseArguments(argv) {
    const options = {};
    for (let index = 0; index < argv.length; index++) {
        if (argv[index] === "--port") options.port = Number(argv[++index]);
        else if (argv[index] === "--isolate") options.isolate = true;
        else if (argv[index] === "--snapshot-dir") options.snapshotDir = argv[++index];
    }
    return options;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
    const options = parseArguments(process.argv.slice(2));
    const running = await startServer({
        ...options,
        onResult: (body) => console.log(`\n--- result ---\n${body}\n`),
    });
    const manifestPath = join(packageRoot, "dist", "component", "manifest.json");
    const manifest = existsSync(manifestPath)
        ? JSON.parse(await readFile(manifestPath, "utf8"))
        : null;

    console.log(`serving   ${running.url}`);
    console.log(`snapshots ${running.snapshots}`);
    console.log(`tier      ${manifest ? manifest.tier : "not built (run npm run build)"}`);
    console.log(`isolation ${options.isolate ? "COOP/COEP on" : "off (offline mode)"}`);
}
