import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { distPath } from "../helpers.mjs";

const { Sandbox } = await import(distPath("index.js"));

function recordingTransport() {
    const calls = [];

    return {
        calls,
        ready: async () => 0,
        terminate() {},
        async call(call) {
            calls.push(call);

            switch (call.kind) {
                case "mount-snapshot":
                    return { snapshotPath: "snap/test.snap", byteLength: 0 };
                case "session-start":
                    return 1n;
                default:
                    return undefined;
            }
        },
    };
}

async function sessionStartFor(mounts) {
    const transport = recordingTransport();
    const sandbox = await Sandbox.create({
        transport,
        network: false,
        snapshot: { bytes: new Uint8Array(4), name: "test-256mb.snap" },
        mounts,
    });

    await sandbox.commands.run("true").catch(() => {});
    return transport.calls.find((call) => call.kind === "session-start");
}

describe("mounts", () => {
    it("carries a plain guest path to the engine as read only", async () => {
        const started = await sessionStartFor({ "/tmp/work": "/workspace" });

        assert.deepEqual(started.mounts, [
            { hostAlias: "/tmp/work", guestPath: "/workspace", writable: false },
        ]);
    });

    it("reads the rw suffix as write access and keeps it out of the path", async () => {
        const started = await sessionStartFor({ "/tmp/work": "/workspace:rw" });

        assert.deepEqual(started.mounts, [
            { hostAlias: "/tmp/work", guestPath: "/workspace", writable: true },
        ]);
    });

    it("carries every directory it was given", async () => {
        const started = await sessionStartFor({ "/a": "/one", "/b": "/two:rw" });

        assert.deepEqual(
            started.mounts.map((mount) => [mount.guestPath, mount.writable]),
            [
                ["/one", false],
                ["/two", true],
            ],
        );
    });

    it("sends an empty list when nothing is mounted", async () => {
        const started = await sessionStartFor(undefined);

        assert.deepEqual(started.mounts, []);
    });

    it("refuses a guest path that is not absolute", async () => {
        await assert.rejects(sessionStartFor({ "/tmp/work": "workspace" }), /absolute guest path/);
    });

    it("refuses an empty guest path", async () => {
        await assert.rejects(sessionStartFor({ "/tmp/work": "" }), /needs a guest path/);
    });

    it("refuses a list where an object of paths belongs", async () => {
        await assert.rejects(sessionStartFor(["/tmp/work"]), /must be an object/);
    });
});
