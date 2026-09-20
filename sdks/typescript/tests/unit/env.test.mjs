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

async function sessionStartFor(env) {
    const transport = recordingTransport();
    const sandbox = await Sandbox.create({
        transport,
        network: false,
        snapshot: { bytes: new Uint8Array(4), name: "test-256mb.snap" },
        env,
    });

    await sandbox.commands.run("true").catch(() => {});
    return transport.calls.find((call) => call.kind === "session-start");
}

describe("env", () => {
    it("carries a variable to the engine", async () => {
        const started = await sessionStartFor({ TZ: "UTC" });

        assert.deepEqual(started.env, [{ name: "TZ", value: "UTC" }]);
    });

    it("keeps values with shell syntax exactly as given", async () => {
        const hostile = "'; echo pwned; x='$HOME";
        const started = await sessionStartFor({ K: hostile });

        assert.deepEqual(started.env, [{ name: "K", value: hostile }]);
    });

    it("carries every variable it was given, in order", async () => {
        const started = await sessionStartFor({ A: "1", B: "2" });

        assert.deepEqual(
            started.env.map((entry) => entry.name),
            ["A", "B"],
        );
    });

    it("sends an empty list when nothing is set", async () => {
        const started = await sessionStartFor(undefined);

        assert.deepEqual(started.env, []);
    });

    it("refuses a name that is not a plain identifier", async () => {
        await assert.rejects(sessionStartFor({ "NOT AN IDENT": "x" }), /plain identifier/);
        await assert.rejects(sessionStartFor({ "1LEADING": "x" }), /plain identifier/);
        await assert.rejects(sessionStartFor({ "SEMI;COLON": "x" }), /plain identifier/);
    });

    it("refuses a value that is not a string", async () => {
        await assert.rejects(sessionStartFor({ PORT: 8080 }), /needs a string value/);
    });

    it("refuses a list where an object of names belongs", async () => {
        await assert.rejects(sessionStartFor(["TZ=UTC"]), /must be an object/);
    });
});
