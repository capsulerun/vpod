import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";

import { distPath, locateSnapshot, skipReason } from "../helpers.mjs";

const { Sandbox } = await import(distPath("node/index.js"));

const BIG_BYTES = 64 * 1024;

async function withMountedDirectory(spec, body) {
    const host = await mkdtemp(join(tmpdir(), "vpod-mount-"));
    await writeFile(join(host, "notes.txt"), "hello from the host\n");
    await writeFile(join(host, "big.bin"), "A".repeat(BIG_BYTES));
    await mkdir(join(host, "nested"), { recursive: true });
    await writeFile(join(host, "nested", "deep.txt"), "nested\n");

    const sandbox = await Sandbox.create({
        snapshot: { path: locateSnapshot() },
        mounts: { [host]: spec },
    });

    try {
        return await body(sandbox, host);
    } finally {
        await sandbox.close();
        await rm(host, { recursive: true, force: true });
    }
}

describe("mounts", { skip: skipReason() ?? false }, () => {
    it("shows the host directory to the guest", async () => {
        await withMountedDirectory("/workspace", async (sandbox) => {
            const result = await sandbox.commands.run("cat /workspace/notes.txt");
            assert.equal(result.stdout.trim(), "hello from the host");
        });
    });

    it("reads a file larger than one page", async () => {
        await withMountedDirectory("/workspace", async (sandbox) => {
            const result = await sandbox.commands.run("wc -c < /workspace/big.bin");
            assert.equal(result.stdout.trim(), String(BIG_BYTES));
        });
    });

    it("walks into subdirectories", async () => {
        await withMountedDirectory("/workspace", async (sandbox) => {
            const result = await sandbox.commands.run("cat /workspace/nested/deep.txt");
            assert.equal(result.stdout.trim(), "nested");
        });
    });

    it("keeps a plain mount read only", async () => {
        await withMountedDirectory("/workspace", async (sandbox, host) => {
            const result = await sandbox.commands.run("echo changed > /workspace/notes.txt");

            assert.notEqual(result.exitCode, 0);
            assert.equal(
                await readFile(join(host, "notes.txt"), "utf8"),
                "hello from the host\n",
            );
        });
    });

    it("writes back to the host when the mount says rw", async () => {
        await withMountedDirectory("/workspace:rw", async (sandbox, host) => {
            const result = await sandbox.commands.run("echo written > /workspace/from-guest.txt");
            assert.equal(result.exitCode, 0);

            assert.equal(await readFile(join(host, "from-guest.txt"), "utf8"), "written\n");
        });
    });

    it("refuses to pretend it mounted anything without a host filesystem", async () => {
        const { createInlineTransport } = await import(distPath("index.js"));

        await assert.rejects(
            Sandbox.create({
                transport: await createInlineTransport(),
                snapshot: { path: locateSnapshot() },
                mounts: { [tmpdir()]: "/workspace" },
            }).then((sandbox) => sandbox.commands.run("true").finally(() => sandbox.close())),
            /mounts need a host filesystem/,
        );
    });

    it("says so when the host directory is missing", async () => {
        await assert.rejects(
            Sandbox.create({
                snapshot: { path: locateSnapshot() },
                mounts: { "/no/such/directory": "/workspace" },
            }).then((sandbox) => sandbox.commands.run("true").finally(() => sandbox.close())),
            /does not exist/,
        );
    });
});
