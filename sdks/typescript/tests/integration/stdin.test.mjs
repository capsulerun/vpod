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

    it("leaves the session usable after a nested shell", async () => {

        await withSandbox(async (sandbox) => {
            const { readable, writer } = pipe();
            const nested = sandbox.commands.run("sh", {
                stdin: readable,
                tty: true,
                timeout: 30,
            });
            await new Promise((resolve) => setTimeout(resolve, 1500));
            await writer.write("exit\n");
            assert.equal((await nested).exitCode, 0);

            const after = await sandbox.commands.run("expr 2 + 2", { timeout: 20 });
            assert.equal(after.stdout, "4");
            assert.equal(after.exitCode, 0);
        });
    });

    it("keeps the prompt out of child processes", async () => {
        await withSandbox(async (sandbox) => {
            const child = await sandbox.commands.run(`sh -c 'echo "[$PS1]"'`, {
                timeout: 20,
            });
            assert.ok(!child.stdout.includes("__ec"), child.stdout);
        });
    });

    it("ends the input even on a terminal when given a string", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("cat", {
                stdin: "hello\n",
                tty: true,
                timeout: 30,
            });
            assert.equal(result.exitCode, 0, "a string is finite, so it has to end");
            assert.ok(result.stdout.includes("hello"));
        });
    });

    it("keeps the newline the program wrote in the last chunk", async () => {
        await withSandbox(async (sandbox) => {
            const chunks = [];
            const result = await sandbox.commands.run("/bin/echo hi", {
                onStdout: (chunk) => chunks.push(chunk),
                timeout: 20,
            });
            assert.deepEqual(chunks, ["hi\n"], "the last chunk used to arrive trimmed");
            assert.equal(result.stdout, "hi");
        });
    });

    it("abandons a stream with a reason the caller can read", async () => {
        await withSandbox(async (sandbox) => {
            const { readable, writer } = pipe();
            const running = sandbox.commands.run("head -1", {
                stdin: readable,
                timeout: 30,
            });
            await writer.write("kept\n");
            await running;

            await assert.rejects(
                () => writer.write("late\n"),
                (thrown) => thrown instanceof Error && thrown.message.includes("vpod:"),
            );
        });
    });

    it("does not run input the command never read", async () => {
        await withSandbox(async (sandbox) => {
            await sandbox.commands.run("rm -f /tmp/leftover-ran", { timeout: 20 });

            const result = await sandbox.commands.run("head -1", {
                stdin: "kept\ntouch /tmp/leftover-ran\n",
                tty: true,
                timeout: 30,
            });
            assert.ok(result.stdout.includes("kept"));

            const verdict = await sandbox.commands.run(
                "if [ -e /tmp/leftover-ran ]; then echo EXECUTED; else echo clean; fi",
                { timeout: 20 },
            );
            assert.equal(verdict.stdout.trim(), "clean", "leftover input reached the shell");
        });
    });

    it("never lets the prompt sentinel reach the caller", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("head -2", {
                stdin: "alpha\nbeta\ngamma\n",
                tty: true,
                timeout: 30,
            });
            assert.ok(!result.stdout.includes("\u001f"), JSON.stringify(result.stdout));

            const after = await sandbox.commands.run("echo alive", { timeout: 20 });
            assert.equal(after.stdout, "alive", JSON.stringify(after.stdout));
        });
    });

    it("still reports the command's own exit code on a terminal", async () => {
        await withSandbox(async (sandbox) => {
            const failed = await sandbox.commands.run("sh -c 'exit 7'", {
                tty: true,
                timeout: 20,
            });
            assert.equal(failed.exitCode, 7);
            const ok = await sandbox.commands.run("true", { tty: true, timeout: 20 });
            assert.equal(ok.exitCode, 0);
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
