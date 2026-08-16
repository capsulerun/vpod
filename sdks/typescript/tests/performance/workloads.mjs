import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const shared = JSON.parse(
    readFileSync(
        resolve(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "performance-workloads.json"),
        "utf8",
    ),
);

export const GUEST_WORKLOADS = shared.workloads;
export const WALL_CEILINGS = shared.wallCeilings;

export const BASELINE_NAME = "perf.json";

export function baselineUrl() {
    const explicit = process.env.VPOD_PERF_BASELINE;
    if (explicit) return explicit;

    const registry = process.env.VPOD_REGISTRY;
    if (!registry) return null;
    return `${registry.slice(0, registry.lastIndexOf("/"))}/${BASELINE_NAME}`;
}

export async function loadBaseline() {
    const url = baselineUrl();
    if (url === null) return {};
    try {
        if (/^https?:\/\//.test(url)) {
            const response = await fetch(url);
            if (!response.ok) throw new Error(`${response.status}`);
            return await response.json();
        }
        return JSON.parse(readFileSync(url, "utf8"));
    } catch (thrown) {
        console.log(`no baseline at ${url}: ${thrown}`);
        return {};
    }
}

export function entryFor(baseline, digest) {
    for (const entry of Object.values(baseline?.snapshots ?? {})) {
        if (Object.values(entry.sha256 ?? {}).includes(digest)) return entry;
    }
    return null;
}

export function guestProgram(body) {
    return `import time\n_t0=time.time()\n${body}\nprint(f'{time.time()-_t0:.6f}')`;
}

export function withinTolerance(measured, expected, tolerance) {
    return Math.abs(measured - expected) / expected <= tolerance;
}
