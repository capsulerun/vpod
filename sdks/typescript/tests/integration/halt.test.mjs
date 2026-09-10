import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { skipReason, withSandbox } from "../helpers.mjs";

describe("guest power-off", { skip: skipReason() ?? false }, () => {
    it("reports a power-off as itself, not as a timeout", async () => {
        await withSandbox(async (sandbox) => {
            await sandbox.code.run("print('warm')");

            const halted = await sandbox.code.run(
                "import os\nos.system('poweroff -f')",
                { timeout: 30 },
            );

            assert.match(halted.error ?? "", /powered off/);
            assert.doesNotMatch(halted.error ?? "", /Timed out/);
        });
    });

    it("refuses further work once the machine is off", async () => {
        await withSandbox(async (sandbox) => {
            await sandbox.code.run("print('warm')");
            await sandbox.code.run("import os\nos.system('poweroff -f')", { timeout: 30 });

            await assert.rejects(
                () => sandbox.code.run("print('x')"),
                /powered itself off/,
            );
        });
    });
});
