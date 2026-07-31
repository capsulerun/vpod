import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const shared = JSON.parse(
    readFileSync(
        resolve(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "performance-workloads.json"),
        "utf8",
    ),
);

export const RECORDED_SNAPSHOT = shared.recordedSnapshot;
export const GUEST_WORKLOADS = shared.workloads;
export const WALL_CEILINGS = shared.wallCeilings;

export function guestProgram(body) {
    return `import time\n_t0=time.time()\n${body}\nprint(f'{time.time()-_t0:.6f}')`;
}

export function withinTolerance(measured, expected, tolerance) {
    return Math.abs(measured - expected) / expected <= tolerance;
}
