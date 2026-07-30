import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repositoryRoot = resolve(packageRoot, "..", "..");

export const distPath = (relativePath) => join(packageRoot, "dist", relativePath);

/**
 * A snapshot the guest can actually load. `dist/prod-bundle` and
 * `dist/registry-bundle` are deliberately excluded: those copies are lz4-framed
 * twice and fail with `invalid snapshot magic`.
 */
export function locateSnapshot() {
    const candidates = [
        process.env.VPOD_TEST_SNAPSHOT,
        join(homedir(), "Library", "Application Support", "vpod", "snapshots", "alpine-3.23.0-256mb.snap"),
        join(homedir(), ".local", "share", "vpod", "snapshots", "alpine-3.23.0-256mb.snap"),
        join(repositoryRoot, "dist", "alpine-3.23.0-256mb.snap"),
    ];

    return candidates.find((candidate) => candidate && existsSync(candidate)) ?? null;
}

export function skipReason() {
    if (!existsSync(distPath("index.js"))) {
        return "dist/ is missing: run npm run build";
    }
    if (locateSnapshot() === null) {
        return "no loadable snapshot found: set VPOD_TEST_SNAPSHOT";
    }
    return null;
}

let cachedModule = null;

export async function loadSdk() {
    cachedModule ??= await import(distPath("index.js"));
    return cachedModule;
}

/**
 * A sandbox running on the calling thread. Node has no `Worker` global and no
 * origin-private storage, so tests supply the transport and the snapshot bytes
 * rather than going through the registry.
 */
export async function createTestSandbox(overrides = {}) {
    const { Sandbox, createInlineTransport } = await loadSdk();
    const snapshotPath = locateSnapshot();

    return Sandbox.create({
        transport: await createInlineTransport(),
        snapshotBytes: readFileSync(snapshotPath),
        snapshotName: basename(snapshotPath),
        ...overrides,
    });
}

export async function withSandbox(body) {
    const sandbox = await createTestSandbox();
    try {
        return await body(sandbox);
    } finally {
        await sandbox.close();
    }
}
