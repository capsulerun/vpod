import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { distPath, skipReason } from "../helpers.mjs";

// Runs against dist like the rest of the unit tests. VPOD_SANDBOX_MODULE points
// it at a scratch build instead, which is how it is driven before a full build.
const modulePath = process.env.VPOD_SANDBOX_MODULE ?? distPath("index.js");
const missing = process.env.VPOD_SANDBOX_MODULE ? null : skipReason();

const { Execution } = missing ? {} : await import(modulePath);

// A dist built before Execution existed would otherwise fail with a confusing
// "not a constructor" on every case.
const stale = missing ?? (typeof Execution !== "function" ? "dist predates Execution" : false);

/**
 * Drives Execution against a fake runtime, so these check the calls the SDK
 * makes rather than guest behaviour. The guest half is an integration test.
 */
function fakeRuntime(slices) {
    const calls = { slices: [], stdin: [], interrupts: 0 };
    let next = 0;

    return {
        calls,
        sessionExecSlice(handle, code, timeout, sliceNanos, mode) {
            calls.slices.push({ code, timeout, mode });
            return Promise.resolve(slices[Math.min(next++, slices.length - 1)]);
        },
        sessionStdin(handle, data) {
            calls.stdin.push(Buffer.from(data));
            return Promise.resolve();
        },
        sessionInterrupt() {
            calls.interrupts++;
            return Promise.resolve();
        },
    };
}

const running = { stdout: "", stderr: "", exitCode: null };
const finished = { stdout: "", stderr: "", exitCode: 0 };

describe("Execution", { skip: stale }, () => {
    it("does not run the command until it is stepped", async () => {
        const runtime = fakeRuntime([finished]);
        const execution = new Execution(runtime, 1n, "echo hi", 120, "closed");

        assert.equal(runtime.calls.slices.length, 0);
        await execution.step();
        assert.equal(runtime.calls.slices.length, 1);
    });

    it("sends the command once and then continues with null", async () => {
        const runtime = fakeRuntime([running, finished]);
        const execution = new Execution(runtime, 1n, "echo hi", 120, "closed");

        await execution.wait();
        assert.equal(runtime.calls.slices[0].code, "echo hi");
        assert.equal(runtime.calls.slices[1].code, null);
    });

    it("flushes queued input before the next slice", async () => {
        const runtime = fakeRuntime([running, finished]);
        const execution = new Execution(runtime, 1n, "cat", 120, "terminal");

        execution.write("one");
        execution.write("two");
        await execution.step();

        assert.equal(runtime.calls.stdin.length, 1, "coalesced into one call");
        assert.equal(runtime.calls.stdin[0].toString(), "onetwo");
    });

    it("sends nothing when there is no input", async () => {
        const runtime = fakeRuntime([finished]);
        await new Execution(runtime, 1n, "echo hi", 120, "closed").wait();
        assert.deepEqual(runtime.calls.stdin, []);
    });

    it("ends a complete line with one Ctrl-D", async () => {
        const runtime = fakeRuntime([running, finished]);
        const execution = new Execution(runtime, 1n, "cat", 120, "piped");

        execution.write("data\n");
        await execution.step();

        assert.equal(runtime.calls.stdin[0].toString("hex"), "646174610a04");
    });

    it("ends a partial line with two, so none falls through to the shell", async () => {
        const runtime = fakeRuntime([running, finished]);
        const execution = new Execution(runtime, 1n, "cat", 120, "piped");

        execution.write("no newline");
        await execution.step();

        assert.ok(runtime.calls.stdin[0].toString("hex").endsWith("0404"));
    });

    it("passes tty through and keeps a zero timeout as zero", async () => {
        const runtime = fakeRuntime([finished]);
        await new Execution(runtime, 1n, "python3", 0, "terminal").wait();

        assert.equal(runtime.calls.slices[0].mode, "terminal");
        assert.equal(runtime.calls.slices[0].timeout, 0n);
    });

    it("interrupts once, not on every slice", async () => {
        const runtime = fakeRuntime([running, running, running, finished]);
        const execution = new Execution(runtime, 1n, "sleep 100", 120, "closed");

        await execution.step();
        execution.interrupt();
        await execution.step();
        await execution.step();

        assert.equal(runtime.calls.interrupts, 1);
    });

    it("refuses to give a result before the command ends", async () => {
        const runtime = fakeRuntime([running]);
        const execution = new Execution(runtime, 1n, "sleep 100", 120, "closed");

        await execution.step();
        assert.throws(() => execution.result(), /still running/);
    });

    it("keeps CRLF in tty mode and normalizes it otherwise", async () => {
        const crlf = { stdout: "a\r\nb", stderr: "", exitCode: 0 };

        const term = new Execution(fakeRuntime([crlf]), 1n, "x", 0, "terminal");
        await term.wait();
        assert.equal(term.stdout, "a\r\nb");

        const plain = new Execution(fakeRuntime([crlf]), 1n, "x", 120, "closed");
        await plain.wait();
        assert.equal(plain.stdout, "a\nb");
    });
});
