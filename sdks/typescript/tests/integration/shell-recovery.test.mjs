import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { createTestSandbox, skipReason } from "../helpers.mjs";

describe("a command that outlives its timeout", { skip: skipReason() ?? false }, () => {

    it("does not take the sandbox with it", async () => {
        const sandbox = await createTestSandbox();
        try {
            await sandbox.commands.run("export MARKER=kept");

            const interrupted = await sandbox.commands.run("sleep 300", { timeout: 3 });
            assert.equal(interrupted.exitCode, 124);

            const after = await sandbox.commands.run("echo $MARKER");
            assert.equal(after.exitCode, 0);
            assert.equal(after.stdout.trim(), "kept");
        } finally {
            await sandbox.close();
        }
    });
});
