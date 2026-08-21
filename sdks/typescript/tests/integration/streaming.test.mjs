import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { skipReason, withSandbox } from "../helpers.mjs";

const TICKS = "for i in 1 2 3 4; do echo line$i; sleep 1; done";

describe("streaming", { skip: skipReason() ?? false }, () => {
    it("delivers output in more than one chunk", async () => {
        await withSandbox(async (sandbox) => {
            const chunks = [];
            const result = await sandbox.commands.run(TICKS, {
                timeout: 60,
                onStdout: (chunk) => chunks.push(chunk),
            });

            assert.equal(result.exitCode, 0);
            assert.equal(result.stdout.trim(), "line1\nline2\nline3\nline4");
            assert.ok(
                chunks.length > 1,
                `everything arrived in one chunk: ${JSON.stringify(chunks)}`,
            );
        });
    });

    it("concatenates to exactly what a callback-free call returns", async () => {
        await withSandbox(async (sandbox) => {
            const chunks = [];
            const streamed = await sandbox.commands.run(TICKS, {
                timeout: 60,
                onStdout: (chunk) => chunks.push(chunk),
            });
            const plain = await sandbox.commands.run(TICKS, { timeout: 60 });

            assert.equal(chunks.join("").trimEnd(), streamed.stdout);
            assert.equal(streamed.stdout, plain.stdout);
        });
    });

    it("keeps stderr on its own callback", async () => {
        await withSandbox(async (sandbox) => {
            const out = [];
            const err = [];
            const result = await sandbox.commands.run("echo to-stdout; echo to-stderr >&2", {
                timeout: 30,
                onStdout: (chunk) => out.push(chunk),
                onStderr: (chunk) => err.push(chunk),
            });

            assert.equal(result.exitCode, 0);
            assert.match(out.join(""), /to-stdout/);
            assert.match(err.join(""), /to-stderr/);
            assert.doesNotMatch(out.join(""), /to-stderr/);
        });
    });

    it("keeps the chunks that arrived before an interrupt", async () => {
        await withSandbox(async (sandbox) => {
            const chunks = [];
            const result = await sandbox.commands.run(
                "for i in $(seq 1 60); do echo tick$i; sleep 1; done",
                {
                    timeout: 120,
                    onStdout: (chunk) => {
                        chunks.push(chunk);
                        if (chunks.length === 2) {
                            void sandbox.commands.interrupt();
                        }
                    },
                },
            );

            assert.equal(result.exitCode, 130, `exited ${result.exitCode}`);
            assert.match(result.stdout, /tick1/);
            assert.ok(chunks.length >= 2, `only ${chunks.length} chunk(s)`);
        });
    });

    it("still returns one chunk for a command that finishes inside a slice", async () => {
        await withSandbox(async (sandbox) => {
            const chunks = [];
            const result = await sandbox.commands.run("echo quick", {
                onStdout: (chunk) => chunks.push(chunk),
            });

            assert.equal(result.stdout.trim(), "quick");
            assert.equal(chunks.length, 1);
        });
    });
});
