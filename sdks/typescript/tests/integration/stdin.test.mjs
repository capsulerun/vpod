import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { skipReason, withSandbox } from "../helpers.mjs";

function pipe() {
    const { readable, writable } = new TransformStream();
    return { readable, writer: writable.getWriter() };
}

describe("stdin", { skip: skipReason() ?? false }, () => {
    it("stays closed unless the caller asks for it", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("cat", { timeout: 20 });
            assert.equal(result.exitCode, 0);
            assert.equal(result.stdout, "");
        });
    });

    it("hands a string to the command", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("cat", {
                stdin: "hello stdin\n",
                timeout: 20,
            });
            assert.equal(result.exitCode, 0);
            assert.equal(result.stdout, "hello stdin");
        });
    });

    it("keeps bytes intact", async () => {
        await withSandbox(async (sandbox) => {
            const payload = new Uint8Array([0, 1, 2, 250, 251, 252, 253, 254, 255]);
            const result = await sandbox.commands.run("wc -c", {
                stdin: payload,
                timeout: 20,
            });
            assert.equal(result.exitCode, 0);
            assert.equal(result.stdout.trim(), String(payload.length));
        });
    });

    it("takes a ReadableStream without being told it is a terminal", async () => {
        await withSandbox(async (sandbox) => {
            const { readable, writer } = pipe();

            const running = sandbox.commands.run("python3 2>&1", {
                stdin: readable,
                timeout: 60,
            });

            await writer.write("print(6 * 7)\n");
            await writer.write("exit()\n");

            const result = await running;
            assert.ok(result.stdout.includes(">>>"), result.stdout);
            assert.ok(result.stdout.includes("42"), result.stdout);
            assert.equal(result.exitCode, 0);
        });
    });

    it("ends the command when the writer closes", async () => {
        await withSandbox(async (sandbox) => {
            const { readable, writer } = pipe();
            let seen = "";

            const running = sandbox.commands.run("cat", {
                stdin: readable,
                timeout: 60,
                onStdout: (chunk) => {
                    seen += chunk;
                },
            });

            await writer.write("first line\n");
            while (!seen.includes("first line")) await new Promise((r) => setTimeout(r, 50));
            await writer.close();

            const result = await running;
            assert.ok(result.stdout.includes("first line"), result.stdout);
            assert.equal(result.exitCode, 0, "closing the writer should end cat");
        });
    });

    it("lets the caller answer what the command prints", async () => {
        await withSandbox(async (sandbox) => {
            const { readable, writer } = pipe();
            const asked = [];
            let seen = "";

            const running = sandbox.commands.run("python3 2>&1", {
                stdin: readable,
                timeout: 60,
                onStdout: (chunk) => {
                    seen += chunk;
                    if (asked.length === 0 && seen.includes(">>>")) {
                        asked.push("prompt");
                        void writer.write("print(6 * 7)\n");
                    } else if (asked.length === 1 && seen.includes("42")) {
                        asked.push("answer");
                        void writer.close();
                    }
                },
            });

            const result = await running;
            assert.deepEqual(asked, ["prompt", "answer"], result.stdout);
            assert.equal(result.exitCode, 0);
        });
    });

    it("does not hang when the command ends before the stream does", async () => {
        await withSandbox(async (sandbox) => {
            const { readable, writer } = pipe();

            const running = sandbox.commands.run("head -1", {
                stdin: readable,
                timeout: 60,
            });

            await writer.write("kept\ndropped\n");

            const result = await running;
            assert.ok(result.stdout.includes("kept"), result.stdout);
        });
    });

    it("takes an async iterable too", async () => {
        await withSandbox(async (sandbox) => {
            async function* lines() {
                yield "one\n";
                yield "two\n";
            }

            const result = await sandbox.commands.run("cat", {
                stdin: lines(),
                timeout: 60,
            });
            assert.equal(result.stdout.replace(/\r/g, "").trim(), "one\ntwo");
            assert.equal(result.exitCode, 0);
        });
    });
});
