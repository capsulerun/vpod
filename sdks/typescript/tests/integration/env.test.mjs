import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { createTestSandbox, skipReason } from "../helpers.mjs";

async function withEnv(env, body) {
    const sandbox = await createTestSandbox({ env });

    try {
        return await body(sandbox);
    } finally {
        await sandbox.close();
    }
}

describe("env", { skip: skipReason() ?? false }, () => {
    it("reaches a command", async () => {
        await withEnv({ VPOD_GREETING: "hello" }, async (sandbox) => {
            const result = await sandbox.commands.run("echo $VPOD_GREETING");

            assert.equal(result.stdout.trim(), "hello");
        });
    });

    it("keeps a value with shell syntax literal", async () => {
        const hostile = "'; echo pwned; x='$HOME `id`";

        await withEnv({ VPOD_HOSTILE: hostile }, async (sandbox) => {
            const result = await sandbox.commands.run('printf %s "$VPOD_HOSTILE"');
            assert.equal(result.stdout, hostile);
        });
    });

    it("reaches a child process", async () => {
        await withEnv({ VPOD_GREETING: "hello" }, async (sandbox) => {
            const result = await sandbox.commands.run("sh -c 'echo $VPOD_GREETING'");

            assert.equal(result.stdout.trim(), "hello");
        });
    });

    it("reaches code.run", async () => {
        await withEnv({ VPOD_GREETING: "hello" }, async (sandbox) => {
            const result = await sandbox.code.run(
                "import os; print(os.environ['VPOD_GREETING'])",
            );

            assert.equal(result.text.trim(), "hello");
        });
    });

    it("survives a value longer than one shell line", async () => {
        const value = "abc'def".repeat(400);

        await withEnv({ VPOD_BIG: value }, async (sandbox) => {
            const result = await sandbox.commands.run('printf %s "$VPOD_BIG" | wc -c');

            assert.equal(result.stdout.trim(), String(value.length));
        });
    });

    it("leaves a sandbox without env untouched", async () => {
        await withEnv(undefined, async (sandbox) => {
            const result = await sandbox.commands.run("echo ${VPOD_GREETING:-unset}");

            assert.equal(result.stdout.trim(), "unset");
        });
    });
});
