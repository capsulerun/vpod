import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { skipReason, withSandbox } from "../helpers.mjs";


const FOREVER = "sleep 300";
const TIMEOUT = 60;

describe("interrupt", { skip: skipReason() ?? false }, () => {


    it("stops a running command with 130, not a timeout", async () => {
        await withSandbox(async (sandbox) => {
            const started = Date.now();
            const pending = sandbox.commands.run(FOREVER, { timeout: TIMEOUT });
            setTimeout(() => void sandbox.commands.interrupt(), 500);

            const result = await pending;
            const elapsed = (Date.now() - started) / 1000;

            assert.equal(result.exitCode, 130);
            assert.ok(elapsed < 30, `took ${elapsed.toFixed(2)}s, so it was not interrupted`);
        });
    });

    it("keeps the output the command produced before it was stopped", async () => {
        await withSandbox(async (sandbox) => {
            const pending = sandbox.commands.run(`echo working; ${FOREVER}`, { timeout: TIMEOUT });
            setTimeout(() => void sandbox.commands.interrupt(), 500);

            const result = await pending;
            assert.equal(result.exitCode, 130);
            assert.equal(result.stdout.trim(), "working");
        });
    });

    it("leaves the session usable", async () => {
        await withSandbox(async (sandbox) => {
            await sandbox.commands.run("export MARKER=kept");

            const pending = sandbox.commands.run(FOREVER, { timeout: TIMEOUT });
            setTimeout(() => void sandbox.commands.interrupt(), 500);
            await pending;

            const after = await sandbox.commands.run("echo $MARKER");
            assert.equal(after.exitCode, 0);
            assert.equal(after.stdout.trim(), "kept");
        });
    });

    it("is a no-op when nothing is running", async () => {
        await withSandbox(async (sandbox) => {
            await sandbox.commands.run("true");
            await sandbox.commands.interrupt();
            await sandbox.commands.interrupt();

            const after = await sandbox.commands.run("echo clean");
            assert.equal(after.exitCode, 0);
            assert.equal(after.stdout.trim(), "clean");
        });
    });

    it("stops on an AbortSignal, and rejects the way one is expected to", async () => {
        await withSandbox(async (sandbox) => {
            const controller = new AbortController();
            setTimeout(() => controller.abort(), 500);

            await assert.rejects(
                sandbox.commands.run(FOREVER, { timeout: TIMEOUT, signal: controller.signal }),
                (thrown) => thrown.name === "AbortError",
            );

            const after = await sandbox.commands.run("echo alive");
            assert.equal(after.stdout.trim(), "alive");
        });
    });

    it("composes with AbortSignal.timeout()", async () => {
        await withSandbox(async (sandbox) => {
            await assert.rejects(
                sandbox.commands.run(FOREVER, { timeout: TIMEOUT, signal: AbortSignal.timeout(500) }),
                (thrown) => thrown.name === "TimeoutError",
            );
        });
    });

    it("refuses a command whose signal has already aborted", async () => {
        await withSandbox(async (sandbox) => {
            await assert.rejects(
                sandbox.commands.run("echo never", { signal: AbortSignal.abort() }),
                (thrown) => thrown.name === "AbortError",
            );
        });
    });

    it("closes stdin, so a reader gets EOF instead of hanging", async () => {
        const cases = [
            ["echo a; read x", 1, "a"],
            ["echo a; cat", 0, "a"],
            ["read x", 1, ""],
            ["python3", 0, ""],
        ];

        await withSandbox(async (sandbox) => {
            for (const [command, exitCode, stdout] of cases) {
                const started = Date.now();
                const result = await sandbox.commands.run(command, { timeout: 10 });
                const elapsed = (Date.now() - started) / 1000;

                assert.equal(result.exitCode, exitCode, `${command} exited ${result.exitCode}`);
                assert.equal(result.stdout.trim(), stdout, `${command} printed ${result.stdout}`);
                assert.ok(elapsed < 5, `${command} waited ${elapsed.toFixed(2)}s, stdin was open`);
            }
        });
    });


    it("stops a command that cannot finish on its own", async () => {
        await withSandbox(async (sandbox) => {
            for (const command of [FOREVER, "awk 'BEGIN{for(i=0;i<100000000;i++)x+=i}'"]) {
                const started = Date.now();
                const pending = sandbox.commands.run(command, { timeout: TIMEOUT });
                setTimeout(() => void sandbox.commands.interrupt(), 500);

                const result = await pending;
                const elapsed = (Date.now() - started) / 1000;

                assert.equal(result.exitCode, 130, `${command} exited ${result.exitCode}`);
                assert.ok(elapsed < 30, `${command} took ${elapsed.toFixed(2)}s, so it ran on`);
            }

            const after = await sandbox.commands.run("echo alive");
            assert.equal(after.stdout.trim(), "alive");
        });
    });

    // Nothing may conclude a command has ended without the command saying so.
    it("lets an unfinishable command have the timeout its caller asked for", async () => {
        await withSandbox(async (sandbox) => {
            const started = Date.now();
            const result = await sandbox.commands.run("echo a; sleep 300", { timeout: 3 });
            const elapsed = (Date.now() - started) / 1000;

            assert.equal(result.exitCode, 124);
            assert.equal(result.stdout.trim(), "a", "output before the wait is still reported");
            assert.ok(elapsed >= 2.5, `returned after ${elapsed.toFixed(2)}s, so something guessed`);
            assert.ok(elapsed < 15, `took ${elapsed.toFixed(2)}s, far past the timeout asked for`);

            const after = await sandbox.commands.run("echo alive");
            assert.equal(after.stdout.trim(), "alive");
        });
    });
});
