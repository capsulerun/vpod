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

async function sessionStartFor(secrets) {
    const transport = recordingTransport();
    const sandbox = await Sandbox.create({
        transport,
        network: false,
        snapshot: { bytes: new Uint8Array(4), name: "test-256mb.snap" },
        secrets,
    });

    await sandbox.commands.run("true").catch(() => {});
    return transport.calls.find((call) => call.kind === "session-start");
}

const KEY = "sk-ant-the-real-thing";

describe("secrets", () => {
    it("carries the value and its hosts to the engine", async () => {
        const started = await sessionStartFor({
            ANTHROPIC_API_KEY: { value: KEY, hosts: ["api.anthropic.com"] },
        });

        assert.equal(started.secrets.length, 1);
        assert.equal(started.secrets[0].name, "ANTHROPIC_API_KEY");
        assert.equal(started.secrets[0].value, KEY);
        assert.deepEqual(started.secrets[0].hosts, ["api.anthropic.com"]);
    });

    it("generates a placeholder that is not the value", async () => {
        const started = await sessionStartFor({
            ANTHROPIC_API_KEY: { value: KEY, hosts: ["api.anthropic.com"] },
        });
        const { placeholder } = started.secrets[0];

        assert.match(placeholder, /^vpod-secret-anthropic_api_key-[0-9a-f]{8}$/);
        assert.notEqual(placeholder, KEY);
    });

    it("gives two sandboxes different placeholders for the same secret", async () => {
        const spec = { K: { value: KEY, hosts: ["api.anthropic.com"] } };
        const [first, second] = await Promise.all([
            sessionStartFor(spec),
            sessionStartFor(spec),
        ]);

        assert.notEqual(first.secrets[0].placeholder, second.secrets[0].placeholder);
    });

    it("keeps a placeholder the caller chose, for clients that check key shape", async () => {
        const started = await sessionStartFor({
            K: { value: KEY, hosts: ["api.anthropic.com"], placeholder: "sk-ant-stand-in" },
        });

        assert.equal(started.secrets[0].placeholder, "sk-ant-stand-in");
    });

    it("takes a single host without a list", async () => {
        const started = await sessionStartFor({
            K: { value: KEY, hosts: "api.anthropic.com" },
        });

        assert.deepEqual(started.secrets[0].hosts, ["api.anthropic.com"]);
    });

    it("never sends the value as an ordinary environment variable", async () => {
        const started = await sessionStartFor({
            ANTHROPIC_API_KEY: { value: KEY, hosts: ["api.anthropic.com"] },
        });

        assert.deepEqual(started.env, [], "the value rode along as plain env");
    });

    it("sends an empty list when nothing is bound", async () => {
        const started = await sessionStartFor(undefined);

        assert.deepEqual(started.secrets, []);
    });

    it("refuses a secret with no host, which could never be spent", async () => {
        await assert.rejects(sessionStartFor({ K: { value: KEY, hosts: [] } }), /at least one host/);
        await assert.rejects(sessionStartFor({ K: { value: KEY } }), /at least one host/);
    });

    it("refuses an empty value", async () => {
        await assert.rejects(
            sessionStartFor({ K: { value: "", hosts: ["api.anthropic.com"] } }),
            /non-empty string value/,
        );
    });

    it("refuses a name that is not a plain identifier", async () => {
        await assert.rejects(
            sessionStartFor({ "NOT AN IDENT": { value: KEY, hosts: ["h"] } }),
            /plain identifier/,
        );
    });
});
