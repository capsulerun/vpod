import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { createTestSandbox, skipReason } from "../helpers.mjs";

const TIMEOUT_SECONDS = 3;

const OFFENDERS = [
    ["sleep 30", "an ordinary long command"],
    ['echo "', "an unterminated quote"],
    ["sh", "a nested shell"],
    ["python3", "an interactive interpreter that ignores Ctrl-C"],
];

describe("recovering from a command that holds the terminal", { skip: skipReason() ?? false }, () => {
    it("times out and leaves the session usable, every time", async () => {
        const sandbox = await createTestSandbox();
        try {
            await sandbox.commands.run("export MARKER=kept");

            for (const [offender, description] of OFFENDERS) {
                const interrupted = await sandbox.commands.run(offender, {
                    timeout: TIMEOUT_SECONDS,
                });
                assert.equal(
                    interrupted.exitCode,
                    124,
                    `${description} (${offender}) should report a timeout`,
                );

                const after = await sandbox.commands.run("echo $MARKER");
                assert.equal(
                    after.stdout.trim(),
                    "kept",
                    `the shell should still answer after ${description} (${offender})`,
                );
            }
        } finally {
            await sandbox.close();
        }
    });

    it("leaves code.run working, since it runs in its own session", async () => {
        const sandbox = await createTestSandbox();
        try {
            await sandbox.commands.run("python3", { timeout: TIMEOUT_SECONDS });
            const execution = await sandbox.code.run("print(40 + 2)");
            assert.equal(execution.text, "42");
        } finally {
            await sandbox.close();
        }
    });
});
