import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { skipReason, withSandbox } from "../helpers.mjs";

describe("entropy", { skip: skipReason() ?? false }, () => {
    it("does not hand two sandboxes the same random stream", async () => {
        const probe =
            "import os, secrets, uuid\n" +
            "print(os.urandom(16).hex(), secrets.token_hex(16), uuid.uuid4())";

        const samples = [];
        for (let round = 0; round < 2; round += 1) {
            await withSandbox(async (sandbox) => {
                const result = await sandbox.code.run(probe);
                assert.equal(result.success, true, `the probe failed: ${result.error}`);
                samples.push(result.text.trim());
            });
        }

        const [first, second] = samples;
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
        const samples = [];
        for (let round = 0; round < 2; round += 1) {
            await withSandbox(async (sandbox) => {
                const result = await sandbox.commands.run(
                    "head -c 16 /dev/urandom | od -An -tx1",
                );
                assert.equal(result.exitCode, 0, result.stderr);
                samples.push(result.stdout.trim());
            });
        }

        const [first, second] = samples;
        assert.ok(first, "the read returned nothing");
        assert.notEqual(first, second, `both shells read ${first} from /dev/urandom`);
    });
});
