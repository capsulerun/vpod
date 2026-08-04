import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { skipReason, withSandbox } from "../helpers.mjs";

/**
 * Kept out of the default integration run because every assertion here waits on
 * a timeout that does not honour its own deadline.
 */
describe("a dead session", { skip: skipReason() ?? false }, () => {
    it("times out every later command, well past the requested deadline", async () => {
        await withSandbox(async (sandbox) => {
            assert.equal((await sandbox.commands.run("exit 3")).exitCode, 3);

            const startedAt = performance.now();
            const after = await sandbox.commands.run("echo after", { timeout: 1 });
            const waited = (performance.now() - startedAt) / 1000;

            assert.equal(after.exitCode, 124);
            assert.equal(after.stdout, "");
            assert.ok(
                waited > 5,
                `a 1s timeout on a dead shell is expected to overrun badly; waited ${waited.toFixed(1)}s. ` +
                    `If this now finishes near 1s, the emulator has been fixed and this test should move ` +
                    `back into tests/integration.`,
            );
        });
    });
});

describe("recovery after a timeout", { skip: skipReason() ?? false }, () => {
    it("costs several seconds on the next command", async () => {
        await withSandbox(async (sandbox) => {
            await sandbox.commands.run("sleep 30", { timeout: 1 });

            const startedAt = performance.now();
            const result = await sandbox.commands.run("echo recovered");
            const waited = (performance.now() - startedAt) / 1000;

            assert.equal(result.stdout.trim(), "recovered");
            console.log(`    resync after a timeout took ${waited.toFixed(2)}s`);
        });
    });
});
