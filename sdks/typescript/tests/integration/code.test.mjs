import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { skipReason, withSandbox } from "../helpers.mjs";

describe("code", { skip: skipReason() ?? false }, () => {
    it("returns printed output as text", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.code.run("print(2 + 2)");
            assert.equal(result.text, "4");
            assert.equal(result.success, true);
        });
    });

    it("keeps variables across calls", async () => {
        await withSandbox(async (sandbox) => {
            await sandbox.code.run("data = [1, 2, 3]");
            const result = await sandbox.code.run("print(sum(data))");
            assert.equal(result.text, "6");
        });
    });

    it("keeps imports across calls", async () => {
        await withSandbox(async (sandbox) => {
            await sandbox.code.run("import json");
            await sandbox.code.run("payload = {'key': 'value'}");
            const result = await sandbox.code.run("print(json.dumps(payload))");
            assert.equal(result.success, true);
            assert.match(result.text, /"key": "value"/);
        });
    });

    it("keeps a function definition across calls", async () => {
        await withSandbox(async (sandbox) => {
            await sandbox.code.run("def double(value):\n    return value * 2\n");
            const result = await sandbox.code.run("print(double(21))");
            assert.equal(result.text, "42");
        });
    });

    it("keeps a class definition across calls", async () => {
        await withSandbox(async (sandbox) => {
            await sandbox.code.run("class Counter:\n    def __init__(self):\n        self.n = 0\n");
            await sandbox.code.run("counter = Counter()");
            await sandbox.code.run("counter.n += 5");
            const result = await sandbox.code.run("print(counter.n)");
            assert.equal(result.text, "5");
        });
    });

    it("reports an exception as an error", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.code.run("1 / 0");
            assert.equal(result.success, false);
            assert.match(result.error, /ZeroDivisionError/);
        });
    });

    it("lets Python handle its own exceptions without reporting failure", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.code.run(
                "try:\n    1 / 0\nexcept ZeroDivisionError:\n    print('caught')\n",
            );
            assert.equal(result.text, "caught");
            assert.equal(result.success, true);
        });
    });

    it("does not read a printed word as a failure", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.code.run(
                "print('Error handling is hard')\nprint('config not found, using defaults')\n",
            );
            assert.equal(result.success, true, `reported: ${result.error}`);
            assert.equal(result.error, null);
        });
    });

    it("reports a non-zero exit even when nothing was printed", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.code.run("import sys\nsys.exit(3)\n");
            assert.equal(result.success, false);
            assert.match(result.error, /3/);
        });
    });

    it("evaluates a list comprehension", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.code.run("print([x**2 for x in range(5)])");
            assert.equal(result.text, "[0, 1, 4, 9, 16]");
        });
    });

    it("handles dicts and sets", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.code.run(
                "print(sorted({'b': 2, 'a': 1}.items()), sorted({3, 1, 2}))",
            );
            assert.equal(result.text, "[('a', 1), ('b', 2)] [1, 2, 3]");
        });
    });

    it("handles string operations", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.code.run("print('-'.join(reversed('abc')))");
            assert.equal(result.text, "c-b-a");
        });
    });

    it("keeps integer arithmetic exact beyond 64 bits", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.code.run("print(2**70 + 1)");
            assert.equal(result.text, "1180591620717411303425");
        });
    });

    it("reports the guest Python as riscv64", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.code.run("import platform; print(platform.machine())");
            assert.equal(result.text, "riscv64");
        });
    });

    it("produces several log lines for several prints", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.code.run("print('one')\nprint('two')\n");
            assert.deepEqual(result.logs, ["one", "two"]);
        });
    });

    it("returns 124-style failure when code outlives its timeout", async () => {
        await withSandbox(async (sandbox) => {
            const result = await sandbox.code.run("import time; time.sleep(30)", { timeout: 1 });
            assert.equal(result.success, false);
            assert.match(result.error, /Timed out after 1s/);
        });
    });

    it("keeps the session usable after a code timeout", async () => {
        await withSandbox(async (sandbox) => {
            await sandbox.code.run("import time; time.sleep(30)", { timeout: 1 });
            const result = await sandbox.code.run("print('recovered')");
            assert.equal(result.text, "recovered");
        });
    });
});

describe("code and commands share one machine", { skip: skipReason() ?? false }, () => {
    it("lets Python read what the shell wrote", async () => {
        await withSandbox(async (sandbox) => {
            await sandbox.commands.run("echo from-shell > /tmp/shared.txt");
            const result = await sandbox.code.run("print(open('/tmp/shared.txt').read().strip())");
            assert.equal(result.text, "from-shell");
        });
    });

    it("lets the shell read what Python wrote", async () => {
        await withSandbox(async (sandbox) => {
            await sandbox.code.run("open('/tmp/from-python.txt', 'w').write('from-python')");
            const result = await sandbox.commands.run("cat /tmp/from-python.txt");
            assert.equal(result.stdout.trim(), "from-python");
        });
    });
});
