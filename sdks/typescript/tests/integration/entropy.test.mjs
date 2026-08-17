import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { skipReason, withSandbox } from "../helpers.mjs";

async function sampleTwice(take) {
    const samples = [];
    for (let round = 0; round < 2; round += 1) {
        await withSandbox(async (sandbox) => {
            samples.push(await take(sandbox));
        });
    }
    return samples;
}

describe("entropy", { skip: skipReason() ?? false }, () => {
    it("does not hand two sandboxes the same random stream", async () => {
        const probe =
            "import os, secrets, uuid\n" +
            "print(os.urandom(16).hex(), secrets.token_hex(16), uuid.uuid4())";

        const [first, second] = await sampleTwice(async (sandbox) => {
            const result = await sandbox.code.run(probe);
            assert.equal(result.success, true, `the probe failed: ${result.error}`);
            return result.text.trim();
        });

        assert.ok(first, "the probe printed nothing");
        assert.notEqual(
            first,
            second,
            `both sandboxes produced ${first}. The guest crng was restored from the ` +
                `snapshot and never re-keyed, so every sandbox from this image shares ` +
                `one stream of uuids, tokens and keys.`,
        );
    });

    it("reseeds the shell as well as the interpreter", async () => {
        const [first, second] = await sampleTwice(async (sandbox) => {
            const result = await sandbox.commands.run("head -c 16 /dev/urandom | od -An -tx1");
            assert.equal(result.exitCode, 0, result.stderr);
            return result.stdout.trim();
        });

        assert.ok(first, "the read returned nothing");
        assert.notEqual(first, second, `both shells read ${first} from /dev/urandom`);
    });

    it("does not hand two sandboxes the same random.random() sequence", async () => {
        const [first, second] = await sampleTwice(async (sandbox) => {
            const result = await sandbox.code.run("import random\nprint(random.random())");
            assert.equal(result.success, true, `the probe failed: ${result.error}`);
            return result.text.trim();
        });

        assert.ok(first, "the probe printed nothing");
        assert.notEqual(
            first,
            second,
            `both sandboxes produced ${first}. The interpreter was restored with random ` +
                `already imported, so its Mersenne Twister state came from the snapshot.`,
        );
    });

    it("does not hand two sandboxes the same numpy global stream", async (t) => {
        const probe =
            "try:\n" +
            "    import numpy\n" +
            "except ImportError:\n" +
            "    print('NO_NUMPY')\n" +
            "else:\n" +
            "    print(numpy.random.rand(), numpy.random.randint(0, 10**9))";

        const [first, second] = await sampleTwice(async (sandbox) => {
            const result = await sandbox.code.run(probe, { timeout: 180 });
            assert.equal(result.success, true, `the probe failed: ${result.error}`);
            return result.text.trim();
        });

        if (first === "NO_NUMPY") {
            t.skip("this snapshot does not carry numpy");
            return;
        }

        assert.notEqual(
            first,
            second,
            `both sandboxes produced ${first}. numpy.random's legacy global was seeded ` +
                `when the snapshot warm-imported it, so reseeding the kernel pool and ` +
                `random alone leaves it shared.`,
        );
    });

    it("does not hand two shells the same $RANDOM sequence", async () => {
        const [first, second] = await sampleTwice(async (sandbox) => {
            const result = await sandbox.commands.run("echo $RANDOM $RANDOM $RANDOM");
            assert.equal(result.exitCode, 0, result.stderr);
            return result.stdout.trim();
        });

        assert.ok(first, "the shell printed nothing");
        assert.notEqual(
            first,
            second,
            `both shells produced ${first}. RANDOM is the shell's own generator, seeded ` +
                `before the snapshot was taken, so it survives a kernel pool reseed.`,
        );
    });

    it("leaves a seed the caller sets on purpose alone", async () => {
        await withSandbox(async (sandbox) => {
            const seeded = await sandbox.code.run("import random\nrandom.seed(42)");
            assert.equal(seeded.success, true, `seeding failed: ${seeded.error}`);

            const first = await sandbox.code.run("print(random.random())");
            assert.equal(first.success, true, `the probe failed: ${first.error}`);

            const reference = await sandbox.code.run(
                "import random\nrandom.seed(42)\nprint(random.random())",
            );

            assert.equal(
                first.text.trim(),
                reference.text.trim(),
                "a reseed ran between two calls in one sandbox and threw away the " +
                    "caller's own random.seed(42)",
            );
        });
    });
});
