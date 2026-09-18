import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { basename } from "node:path";
import { describe, it } from "node:test";

import { createTestSandbox, loadSdk, locateSnapshot, skipReason } from "../helpers.mjs";

function* programs(nodes) {
    for (const node of nodes) {
        yield node;
        yield* programs(node.children);
    }
}

async function withTracedSandbox(body) {
    const sandbox = await createTestSandbox({ trace: true });
    try {
        return await body(sandbox);
    } finally {
        await sandbox.close();
    }
}

describe("trace", { skip: skipReason() ?? false }, () => {
    it("records the commands a shell script runs and the files it writes", async () => {
        await withTracedSandbox(async (sandbox) => {
            const result = await sandbox.commands.run(
                "sh -c 'mkdir -p /tmp/traced && echo hi > /tmp/traced/out.txt && cat /tmp/traced/out.txt'",
            );
            assert.equal(result.exitCode, 0);

            const commands = [...programs(result.trace.processes())].map((process) => process.argv);
            assert.deepEqual(commands[0], [
                "sh",
                "-c",
                "mkdir -p /tmp/traced && echo hi > /tmp/traced/out.txt && cat /tmp/traced/out.txt",
            ]);
            assert.ok(commands.some((argv) => argv[0] === "cat"), JSON.stringify(commands));

            const written = result.trace.files().filter((file) => file.written).map((file) => file.path);
            assert.ok(written.includes("/tmp/traced/out.txt"), JSON.stringify(written));
            assert.equal(result.trace.complete, true);
        });
    });

    it("says which program started which, and where relative paths landed", async () => {
        await withTracedSandbox(async (sandbox) => {
            const script = "mkdir -p /tmp/tree && cd /tmp/tree && cat /etc/hostname > host.txt";
            const result = await sandbox.commands.run(`sh -c '${script}'`);
            assert.equal(result.exitCode, 0);
            assert.equal(result.trace.complete, true);

            const roots = result.trace.processes();
            assert.equal(roots.length, 1, JSON.stringify(roots.map((node) => node.argv)));
            assert.deepEqual(roots[0].argv, ["sh", "-c", script]);
            assert.equal(typeof roots[0].pid, "number");

            const children = new Map(roots[0].children.map((node) => [node.argv[0], node]));
            assert.ok(children.has("mkdir") && children.has("cat"), JSON.stringify([...children.keys()]));
            assert.equal(children.get("cat").exitCode, 0);

            const written = result.trace.files().filter((file) => file.written);
            const copy = written.find((file) => file.path === "/tmp/tree/host.txt");
            assert.ok(copy, JSON.stringify(written.map((file) => file.path)));
            assert.ok(copy.processes.length > 0);
        });
    });

    it("keeps each command's events to that command and the whole run on the sandbox", async () => {
        await withTracedSandbox(async (sandbox) => {
            const first = await sandbox.commands.run("touch /tmp/first");
            const second = await sandbox.commands.run("touch /tmp/second");

            const pathsOf = (trace) => trace.files().map((file) => file.path);
            assert.ok(pathsOf(first.trace).includes("/tmp/first"));
            assert.ok(!pathsOf(first.trace).includes("/tmp/second"));
            assert.ok(pathsOf(second.trace).includes("/tmp/second"));

            const everything = pathsOf(await sandbox.trace.collect());
            assert.ok(everything.includes("/tmp/first") && everything.includes("/tmp/second"));
        });
    });

    it("hides vpod's own plumbing unless asked for it", async () => {
        await withTracedSandbox(async (sandbox) => {
            const result = await sandbox.commands.run("true");

            const visible = result.trace.files({ noise: true }).map((file) => file.path);
            assert.ok(!visible.includes("/dev/ttyS1"), JSON.stringify(visible));

            const everything = result.trace.files({ internal: true, noise: true }).map((file) => file.path);
            assert.ok(everything.includes("/dev/ttyS1"), JSON.stringify(everything));
        });
    });

    it("follows commands live and stops when the sandbox closes", async () => {
        const sandbox = await createTestSandbox({ trace: true });
        const seen = [];
        const following = (async () => {
            for await (const event of sandbox.trace.watch()) seen.push(event);
        })();

        await sandbox.commands.run("touch /tmp/watched");
        await sandbox.close();
        await following;

        assert.ok(
            seen.some((event) => event.kind === "file.open" && event.path === "/tmp/watched"),
            JSON.stringify(seen.map((event) => event.kind)),
        );
    });

    it("forgets what clear drained", async () => {
        await withTracedSandbox(async (sandbox) => {
            await sandbox.commands.run("touch /tmp/cleared");
            await sandbox.trace.clear();
            await sandbox.commands.run("touch /tmp/kept");

            const paths = (await sandbox.trace.collect()).files().map((file) => file.path);
            assert.ok(paths.includes("/tmp/kept"), JSON.stringify(paths));
            assert.ok(!paths.includes("/tmp/cleared"), JSON.stringify(paths));
        });
    });

    it("starts a fresh trace on a resumed sandbox", async () => {
        const sandbox = await createTestSandbox({ trace: true });
        await sandbox.commands.run("touch /tmp/before-suspend");
        const delta = await sandbox.suspend();
        const before = (await sandbox.trace.collect()).files().map((file) => file.path);
        await sandbox.close();
        assert.ok(before.includes("/tmp/before-suspend"), JSON.stringify(before));

        const { Sandbox, createInlineTransport } = await loadSdk();
        const snapshotPath = locateSnapshot();
        const resumed = await Sandbox.resume(
            { id: "test", snapshotId: basename(snapshotPath), delta },
            {
                transport: await createInlineTransport(),
                snapshot: { bytes: readFileSync(snapshotPath), name: basename(snapshotPath) },
                trace: true,
            },
        );
        try {
            const after = await resumed.commands.run("touch /tmp/after-resume");
            assert.ok(after.trace.files().some((file) => file.path === "/tmp/after-resume"));

            const everything = (await resumed.trace.collect()).files().map((file) => file.path);
            assert.ok(!everything.includes("/tmp/before-suspend"), JSON.stringify(everything));
        } finally {
            await resumed.close();
        }
    });

    it("says how to turn tracing on when it is off", async () => {
        const sandbox = await createTestSandbox();
        try {
            const result = await sandbox.commands.run("true");
            assert.throws(() => result.trace, /trace: true/);
            await assert.rejects(sandbox.trace.collect(), /trace: true/);
        } finally {
            await sandbox.close();
        }
    });
});
