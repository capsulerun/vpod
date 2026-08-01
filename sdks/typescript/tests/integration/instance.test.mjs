import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { basename } from "node:path";
import { describe, it } from "node:test";

import { createTestSandbox, loadSdk, locateSnapshot, skipReason } from "../helpers.mjs";

async function resumeFrom(delta) {
    const { Sandbox, createInlineTransport } = await loadSdk();
    const snapshotPath = locateSnapshot();
    const snapshotName = basename(snapshotPath);

    return Sandbox.resume(
        { id: "test", snapshotId: snapshotName, delta },
        {
            transport: await createInlineTransport(),
            snapshot: { bytes: readFileSync(snapshotPath), name: snapshotName },
        },
    );
}

describe("suspend and resume", { skip: skipReason() ?? false }, () => {
    it("returns delta bytes rather than a storage handle", async () => {
        const sandbox = await createTestSandbox();
        try {
            const delta = await sandbox.suspend();
            assert.ok(delta instanceof Uint8Array);
            assert.ok(delta.byteLength > 0, "a delta should not be empty");
        } finally {
            await sandbox.close();
        }
    });

    it("writes a delta far smaller than the snapshot", async () => {
        const sandbox = await createTestSandbox();
        try {
            const delta = await sandbox.suspend();
            const snapshotBytes = readFileSync(locateSnapshot()).byteLength;
            assert.ok(
                delta.byteLength < snapshotBytes / 4,
                `delta ${delta.byteLength} should be much smaller than ${snapshotBytes}`,
            );
        } finally {
            await sandbox.close();
        }
    });

    it("preserves environment variables", async () => {
        const sandbox = await createTestSandbox();
        await sandbox.commands.run("export MARKER=survived");
        const delta = await sandbox.suspend();
        await sandbox.close();

        const resumed = await resumeFrom(delta);
        try {
            const result = await resumed.commands.run("echo $MARKER");
            assert.equal(result.stdout.trim(), "survived");
        } finally {
            await resumed.close();
        }
    });

    it("preserves files", async () => {
        const sandbox = await createTestSandbox();
        await sandbox.commands.run("echo kept > /tmp/persisted.txt");
        const delta = await sandbox.suspend();
        await sandbox.close();

        const resumed = await resumeFrom(delta);
        try {
            const result = await resumed.commands.run("cat /tmp/persisted.txt");
            assert.equal(result.stdout.trim(), "kept");
        } finally {
            await resumed.close();
        }
    });

    it("preserves Python REPL state", async () => {
        const sandbox = await createTestSandbox();
        await sandbox.code.run("import json");
        await sandbox.code.run("payload = {'kept': True}");
        const delta = await sandbox.suspend();
        await sandbox.close();

        const resumed = await resumeFrom(delta);
        try {
            const result = await resumed.code.run("print(json.dumps(payload))");
            assert.equal(result.success, true);
            assert.match(result.text, /"kept": true/);
        } finally {
            await resumed.close();
        }
    });

    it("starts a fresh session after suspending", async () => {
        const sandbox = await createTestSandbox();
        try {
            await sandbox.commands.run("export BEFORE=1");
            await sandbox.suspend();

            const result = await sandbox.commands.run("echo [$BEFORE]");
            assert.equal(result.stdout.trim(), "[]");
        } finally {
            await sandbox.close();
        }
    });

    it("can resume the same delta more than once", async () => {
        const sandbox = await createTestSandbox();
        await sandbox.commands.run("export SHARED=twice");
        const delta = await sandbox.suspend();
        await sandbox.close();

        for (const attempt of [1, 2]) {
            const resumed = await resumeFrom(delta.slice());
            try {
                const result = await resumed.commands.run("echo $SHARED");
                assert.equal(result.stdout.trim(), "twice", `attempt ${attempt}`);
            } finally {
                await resumed.close();
            }
        }
    });
});

describe("instance storage", () => {
    it("explains itself when origin-private storage is unavailable", async () => {
        const { InstanceStore } = await loadSdk();

        if (InstanceStore.available()) {
            return;
        }
        await assert.rejects(() => InstanceStore.open(), /origin-private storage is unavailable/);
    });
});
