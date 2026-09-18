import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, it } from "node:test";

import { distPath } from "../helpers.mjs";

await import(distPath("node/index.js"));
const filesystem = await import("@bytecodealliance/preview2-shim/filesystem");

const CREATE_A_FILE = {
    pathFlags: { symlinkFollow: false },
    openFlags: { create: true, directory: false, exclusive: false, truncate: true },
    descriptorFlags: {
        read: false,
        write: true,
        fileIntegritySync: false,
        dataIntegritySync: false,
        requestedWriteSync: false,
        mutateDirectory: true,
    },
};

function createThrough(directory, name) {
    const [root] = filesystem.preopens.getDirectories()[0];
    const relative = join(directory, name).replace(/^\//, "");

    const descriptor = root.openAt(
        CREATE_A_FILE.pathFlags,
        relative,
        CREATE_A_FILE.openFlags,
        CREATE_A_FILE.descriptorFlags,
    );
    descriptor.write(new TextEncoder().encode("written\n"), 0n);
    descriptor[Symbol.dispose]?.();
}

describe("write access to a mounted directory on Node", () => {
    it("creates a file the guest asked to mutate a directory for", () => {
        const directory = mkdtempSync(join(tmpdir(), "vpod-mount-writes-"));

        try {
            createThrough(directory, "created.txt");
            assert.equal(readFileSync(join(directory, "created.txt"), "utf8"), "written\n");
        } finally {
            rmSync(directory, { recursive: true, force: true });
        }
    });

    it("says whether the shim still needs the retry at all", async () => {
        const untouched = await import(
            `${import.meta.resolve("@bytecodealliance/preview2-shim/filesystem")}?unpatched`
        );

        const directory = mkdtempSync(join(tmpdir(), "vpod-mount-writes-"));
        const [root] = untouched.preopens.getDirectories()[0];
        const relative = join(directory, "direct.txt").replace(/^\//, "");

        let refused = false;
        try {
            const descriptor = root.openAt(
                CREATE_A_FILE.pathFlags,
                relative,
                CREATE_A_FILE.openFlags,
                CREATE_A_FILE.descriptorFlags,
            );
            descriptor[Symbol.dispose]?.();
        } catch (thrown) {
            refused = thrown === "unsupported";
        } finally {
            rmSync(directory, { recursive: true, force: true });
        }

        assert.equal(
            refused,
            true,
            "preview2-shim now accepts mutate-directory, so src/node/mount-writes.ts " +
                "and this test can both go",
        );
    });
});
