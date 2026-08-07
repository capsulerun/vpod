import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { skipReason, withSandbox } from "../helpers.mjs";

describe("commands", { skip: skipReason() ?? false }, () => {
    it("returns stdout", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("echo hello");
            assert.equal(result.stdout.trim(), "hello");
            assert.equal(result.success, true);
        });
    });

    it("reports exit code zero for a command that succeeds", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("true");
            assert.equal(result.exitCode, 0);
        });
    });

    it("reports a nonzero exit code", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("exit 3");
            assert.equal(result.exitCode, 3);
            assert.equal(result.success, false);
        });
    });

    it("reports 127 for a command that does not exist", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("definitely-not-a-command");
            assert.equal(result.exitCode, 127);
        });
    });

    it("keeps environment variables across calls", async () => {
        await withSandbox(async (sandbox) => {
            await sandbox.commands.run("export MARKER=persisted");
            const result = await sandbox.commands.run("echo $MARKER");
            assert.equal(result.stdout.trim(), "persisted");
        });
    });

    it("keeps files across calls", async () => {
        await withSandbox(async (sandbox) => {
            await sandbox.commands.run("echo contents > /tmp/written.txt");
            const result = await sandbox.commands.run("cat /tmp/written.txt");
            assert.equal(result.stdout.trim(), "contents");
        });
    });

    it("keeps the working directory across calls", async () => {
        await withSandbox(async (sandbox) => {
            await sandbox.commands.run("mkdir -p /tmp/somewhere && cd /tmp/somewhere");
            const result = await sandbox.commands.run("pwd");
            assert.equal(result.stdout.trim(), "/tmp/somewhere");
        });
    });

    it("captures stderr", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("echo oops >&2");
            assert.match(result.stderr, /oops/);
        });
    });

    it("keeps stderr out of stdout", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("echo out; echo err >&2");
            assert.match(result.stdout, /out/);
            assert.doesNotMatch(result.stdout, /err/);
        });
    });

    it("reports stderr together with a nonzero exit code", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("echo bad >&2; exit 2");
            assert.equal(result.exitCode, 2);
            assert.match(result.stderr, /bad/);
        });
    });

    it("runs a loop written on one line", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("for n in 1 2 3; do echo $n; done");
            assert.equal(result.success, true);
            assert.deepEqual(result.stdout.trim().split("\n"), ["1", "2", "3"]);
        });
    });

    it("returns only the output when input spans several lines", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run(
                ["total=0", "for n in 1 2 3; do", "  total=$((total + n))", "done", "echo $total"].join(
                    "\n",
                ),
            );

            assert.equal(result.stdout.trim(), "6");
            assert.doesNotMatch(result.stdout, /^>/);
        });
    });

    it("supports pipes", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("printf 'a\\nb\\nc\\n' | grep b");
            assert.equal(result.stdout.trim(), "b");
        });
    });

    it("propagates a subshell exit code without ending the session", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("(exit 5)");
            assert.equal(result.exitCode, 5);

            const after = await sandbox.commands.run("echo alive");
            assert.equal(after.stdout.trim(), "alive");
        });
    });

    it("reports the exit code of a bare exit", async () => {
        await withSandbox(async (sandbox) => {
            assert.equal((await sandbox.commands.run("exit 3")).exitCode, 3);
        });
    });

    it("reports the guest architecture as riscv64", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("uname -m");
            assert.equal(result.stdout.trim(), "riscv64");
        });
    });
});

describe("commands timeouts", { skip: skipReason() ?? false }, () => {
    it("returns 124 when a command outlives its timeout", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("sleep 30", { timeout: 1 });
            assert.equal(result.exitCode, 124);
        });
    });

    it("succeeds when the command finishes inside the timeout", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("echo quick", { timeout: 30 });
            assert.equal(result.stdout.trim(), "quick");
            assert.equal(result.exitCode, 0);
        });
    });

    it("keeps the session usable after a timeout", async () => {
        await withSandbox(async (sandbox) => {
            await sandbox.commands.run("sleep 30", { timeout: 1 });
            const result = await sandbox.commands.run("echo still here");
            assert.equal(result.stdout.trim(), "still here");
        });
    });

    it("runs a heredoc", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("cat <<'EOF'\nalpha\nbeta\nEOF");
            assert.equal(result.exitCode, 0);
            assert.equal(result.stdout.trim(), "alpha\nbeta");
        });
    });

    it("writes a file with a heredoc and runs it back", async () => {
        await withSandbox(async (sandbox) => {
            const written = await sandbox.commands.run(
                "cat <<'EOF' > /tmp/written.py\nprint('written')\nEOF",
            );
            assert.equal(written.exitCode, 0);

            const ran = await sandbox.commands.run("python3 /tmp/written.py");
            assert.equal(ran.stdout.trim(), "written");
        });
    });

    it("expands variables in an unquoted heredoc", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("NAME=world\ncat <<EOF\nhello $NAME\nEOF");
            assert.equal(result.stdout.trim(), "hello world");
        });
    });

    it("keeps stderr on its own stream", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("echo out; echo err >&2");
            assert.equal(result.stdout.trim(), "out");
            assert.equal(result.stderr.trim(), "err");
        });
    });

    it("runs commands that end in a comment", async () => {
        const cases = [
            ["echo #", 0, ""],
            ["ls -d / #", 0, "/"],
            ["pwd #", 0, "/"],
            ["basename /a/b #", 0, "b"],
            ["echo kept # a note after it", 0, "kept"],
            ["echo a; echo b #", 0, "a\nb"],
            ["echo '#' #", 0, "#"],
            // A real non-zero exit
            ["false #", 1, ""],
        ];

        await withSandbox(async (sandbox) => {
            for (const [command, exitCode, stdout] of cases) {
                const result = await sandbox.commands.run(command, { timeout: 10 });
                assert.equal(result.exitCode, exitCode, `${command} exited ${result.exitCode}`);
                assert.equal(result.stdout.trim(), stdout, `${command} printed ${result.stdout}`);
            }
        });
    });

    it("comes back quickly from a command that timed out", async () => {
        await withSandbox(async (sandbox) => {
            const hung = await sandbox.commands.run("cat", { timeout: 3 });
            assert.equal(hung.exitCode, 124);

            const started = Date.now();
            const after = await sandbox.commands.run("echo back");
            const elapsed = (Date.now() - started) / 1000;

            assert.equal(after.stdout.trim(), "back");
            assert.ok(elapsed < 3, `the command after a timeout took ${elapsed.toFixed(2)}s`);
        });
    });
});
