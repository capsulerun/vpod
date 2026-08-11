import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { distPath } from "../helpers.mjs";

const { Sandbox } = await import(distPath("index.js"));

/**
 * Answers the few calls starting a sandbox makes, and records them, so the
 * commands a session issues before the caller's own are visible.
 */
function fakeTransport(networkBackend) {
    const calls = [];
    return {
        calls,
        networkBackend,
        async ready() {
            return 0;
        },
        terminate() {},
        async call(call) {
            calls.push(call);
            switch (call.kind) {
                case "pull-snapshot":
                    return {
                        snapshotPath: "/snapshots/vsnap-base.snap",
                        id: "vsnap-base-256mb",
                        byteLength: 1,
                        source: "opfs",
                        fetchMilliseconds: 0,
                        verifyMilliseconds: 0,
                        storeMilliseconds: 0,
                    };
                case "session-start":
                    return 1n;
                case "session-exec":
                    return { stdout: "", stderr: "", exitCode: 0 };
                case "session-exec-slice":
                    return { stdout: "", stderr: "", exitCode: 0 };
                default:
                    return undefined;
            }
        },
    };
}

const execs = (transport) =>
    transport.calls
        .filter((call) => call.kind === "session-exec" || call.kind === "session-exec-slice")
        .map((call) => call.code)
        .filter((code) => code !== null && code !== undefined);

describe("where apk looks", () => {
    it("is rewritten when the guest reaches the network through the browser", async () => {
        const transport = fakeTransport("fetch");
        const sandbox = await Sandbox.create({ transport });
        await sandbox.commands.run("echo hello");

        const [first] = execs(transport);
        assert.match(first, /\/etc\/apk\/repositories/);
        assert.match(first, /apk-mirror\.vpod\.sh/);
        assert.match(first, /dl-cdn/, "only the host should be replaced");
    });

    it("is left alone under node, which reads the cdn directly", async () => {
        const transport = fakeTransport("sockets");
        const sandbox = await Sandbox.create({ transport });
        await sandbox.commands.run("echo hello");

        assert.deepEqual(execs(transport), ["echo hello"]);
    });

    it("is left alone when the caller opts out", async () => {
        const transport = fakeTransport("fetch");
        const sandbox = await Sandbox.create({ transport, apkMirror: false });
        await sandbox.commands.run("echo hello");

        assert.deepEqual(execs(transport), ["echo hello"]);
    });

    it("honours a mirror the caller chose", async () => {
        const transport = fakeTransport("fetch");
        const sandbox = await Sandbox.create({
            transport,
            apkMirror: "https://mirrors.example.test/alpine",
        });
        await sandbox.commands.run("echo hello");

        assert.match(execs(transport)[0], /mirrors\.example\.test/);
        assert.doesNotMatch(execs(transport)[0], /apk-mirror\.vpod\.sh/);
    });

    it("runs once, not before every command", async () => {
        const transport = fakeTransport("fetch");
        const sandbox = await Sandbox.create({ transport });
        await sandbox.commands.run("one");
        await sandbox.commands.run("two");

        const rewrites = execs(transport).filter((code) => code.includes("/etc/apk/repositories"));
        assert.equal(rewrites.length, 1);
    });
});
