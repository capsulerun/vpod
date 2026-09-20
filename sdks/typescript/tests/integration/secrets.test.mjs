import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { createTestSandbox, skipReason } from "../helpers.mjs";

const REAL = "sk-ant-the-real-thing-do-not-leak";

async function withSecret(secrets, body) {
    const sandbox = await createTestSandbox({ secrets });

    try {
        return await body(sandbox);
    } finally {
        await sandbox.close();
    }
}

describe("secrets", { skip: skipReason() ?? false }, () => {
    it("gives the guest a stand-in and never the value", async () => {
        await withSecret(
            { ANTHROPIC_API_KEY: { value: REAL, hosts: ["api.anthropic.com"] } },
            async (sandbox) => {
                const shown = await sandbox.commands.run('printf %s "$ANTHROPIC_API_KEY"');

                assert.match(shown.stdout, /^vpod-secret-anthropic_api_key-[0-9a-f]{8}$/);
                assert.ok(!shown.stdout.includes(REAL));
            },
        );
    });

    it("keeps the value out of everywhere the guest can look", async () => {
        await withSecret(
            { ANTHROPIC_API_KEY: { value: REAL, hosts: ["api.anthropic.com"] } },
            async (sandbox) => {
                const env = await sandbox.commands.run("env");
                const environ = await sandbox.commands.run("cat /proc/self/environ");
                const code = await sandbox.code.run(
                    "import os; print(os.environ['ANTHROPIC_API_KEY'])",
                );

                assert.ok(!env.stdout.includes(REAL), "the value was in env");
                assert.ok(!environ.stdout.includes(REAL), "the value was in /proc");
                assert.ok(!code.text.includes(REAL), "the value was in the interpreter");
                assert.match(code.text.trim(), /^vpod-secret-/);
            },
        );
    });

    it("uses a placeholder the caller chose", async () => {
        await withSecret(
            {
                API_KEY: {
                    value: REAL,
                    hosts: ["api.example.com"],
                    placeholder: "sk-live-stand-in",
                },
            },
            async (sandbox) => {
                const shown = await sandbox.commands.run('printf %s "$API_KEY"');

                assert.equal(shown.stdout, "sk-live-stand-in");
            },
        );
    });
});
