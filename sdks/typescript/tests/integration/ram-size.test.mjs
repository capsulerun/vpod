import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { basename } from "node:path";
import { describe, it } from "node:test";

import { loadSdk, locateSnapshot, skipReason } from "../helpers.mjs";

/**
* Random name that says nothing about memory to test
 */
const ANONYMOUS = "snap_a422ba54177ff2ee.snap";

const SMALLEST_PLAUSIBLE_SHARE = 0.85;

function snapshotsUnderTest() {
    const extra = (process.env.VPOD_TEST_SNAPSHOTS_EXTRA ?? "")
        .split(",")
        .map((path) => path.trim());

    return [locateSnapshot(), ...extra].filter((path) => path && existsSync(path));
}

function capturedMegabytes(fileName) {
    const match = /(\d+)mb/i.exec(fileName);
    return match === null ? null : Number(match[1]);
}

async function guestMemoryTotalMb(path, mountName) {
    const { Sandbox, createInlineTransport } = await loadSdk();

    const sandbox = await Sandbox.create({
        transport: await createInlineTransport(),
        snapshot: { bytes: readFileSync(path), name: mountName },
    });

    try {
        const result = await sandbox.commands.run("free -m | awk '/^Mem:/ { print $2 }'");
        assert.equal(result.exitCode, 0, result.stderr);

        const total = Number(result.stdout.trim());
        assert.ok(
            Number.isFinite(total) && total > 0,
            `free -m printed ${JSON.stringify(result.stdout)}`,
        );

        return total;
    } finally {
        await sandbox.close();
    }
}

describe("guest RAM size", { skip: skipReason() ?? false }, () => {
    for (const path of snapshotsUnderTest()) {
        const fileName = basename(path);
        const captured = capturedMegabytes(fileName);
        const options = { timeout: 240_000 };

        it(`${fileName}: comes from the snapshot, not the file name`, options, async () => {
            const named = await guestMemoryTotalMb(path, fileName);
            const anonymous = await guestMemoryTotalMb(path, ANONYMOUS);

            assert.equal(
                anonymous,
                named,
                `mounted as ${fileName} the guest saw ${named} MB, but the same bytes ` +
                    `mounted as ${ANONYMOUS} saw ${anonymous} MB. The size is being read ` +
                    `off the file name instead of the snapshot header, so any snapshot ` +
                    `not named after its size gets the wrong machine.`,
            );
        });

        if (captured !== null) {
            it(`${fileName}: matches the size it was captured at`, options, async () => {
                const total = await guestMemoryTotalMb(path, fileName);

                assert.ok(
                    total <= captured && total >= captured * SMALLEST_PLAUSIBLE_SHARE,
                    `a ${captured} MB snapshot restored to a guest that sees ${total} MB`,
                );
            });
        }
    }
});
