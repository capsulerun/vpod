import assert from "node:assert/strict";
import { lookup } from "node:dns/promises";
import { mkdtempSync, rmSync } from "node:fs";
import { connect } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, describe, it } from "node:test";

import { distPath, locateSnapshot, skipReason } from "../helpers.mjs";

const reason = skipReason();

const HOST = "kfuckkfmkyxe0l-tests.vpod.sh";
const TIMEOUT_SECONDS = 15;

function hostCanReach(address, port, milliseconds = 10_000) {
    return new Promise((resolve) => {
        const socket = connect({ host: address, port });
        const settle = (reachable) => {
            socket.destroy();
            resolve(reachable);
        };
        socket.setTimeout(milliseconds, () => settle(false));
        socket.once("connect", () => settle(true));
        socket.once("error", () => settle(false));
    });
}

describe("guest egress", { skip: reason ?? false }, () => {
    let sandbox;
    let cacheDirectory;
    let address = null;

    before(async () => {
        cacheDirectory = mkdtempSync(join(tmpdir(), "vpod-cache-"));

        const { Sandbox, createNodeTransport } = await import(distPath("node/index.js"));
        sandbox = await Sandbox.create({
            transport: await createNodeTransport({ cacheDirectory }),
            snapshot: { path: locateSnapshot() },
            network: true,
        });
        await sandbox.commands.run("echo warmup");

        try {
            address = (await lookup(HOST, { family: 4 })).address;
        } catch {
            address = null;
        }
    });

    after(async () => {
        await sandbox?.close();
        if (cacheDirectory !== undefined) {
            rmSync(cacheDirectory, { recursive: true, force: true });
        }
    });

    const wget = (url, extra = "") =>
        sandbox.commands.run(
            `wget -q -O /dev/null -T ${TIMEOUT_SECONDS} -t 1 ${extra} ${url}`,
            { timeout: TIMEOUT_SECONDS * 2 },
        );

    it("opens a TCP connection out of the guest at all", async (t) => {
        if (address === null) {
            t.skip("the test runner cannot resolve the host, so this proves nothing");
            return;
        }
        if (!(await hostCanReach(address, 80))) {
            t.skip("the test runner cannot reach the host either, so this is the network");
            return;
        }

        const result = await wget(`http://${address}`, `--header 'Host: ${HOST}'`);

        assert.equal(
            result.exitCode,
            0,
            `guest cannot reach ${address}:80 without DNS, but this machine can. ` +
                `That is vpod's socket path, not the network. stderr=${result.stderr}`,
        );
    });

    it("resolves a name from inside the guest", async (t) => {
        if (address === null) {
            t.skip("the test runner cannot resolve the host, so this proves nothing");
            return;
        }

        const byName = await wget(`http://${HOST}`);
        if (byName.exitCode === 0) return;

        const byAddress = await wget(`http://${address}`, `--header 'Host: ${HOST}'`);
        assert.fail(
            byAddress.exitCode === 0
                ? `guest DNS is broken: ${address} answers with a Host header but the name does not resolve`
                : `guest egress is broken: ${address} is unreachable even without DNS`,
        );
    });

    it("completes a TLS handshake through the proxy", async (t) => {
        if (address === null) {
            t.skip("the test runner cannot resolve the host, so this proves nothing");
            return;
        }

        const overTls = await sandbox.commands.run(
            `wget -q -O- -T ${TIMEOUT_SECONDS} -t 1 https://${HOST}`,
            { timeout: TIMEOUT_SECONDS * 2 },
        );

        if (overTls.exitCode !== 0) {
            const plain = await wget(`http://${HOST}`);
            assert.fail(
                plain.exitCode === 0
                    ? `http works but https does not, so this is the TLS proxy. stderr=${overTls.stderr}`
                    : `neither http nor https works, so this is not TLS. stderr=${overTls.stderr}`,
            );
        }

        assert.match(overTls.stdout, /VPOD_TEST_OK/);
    });
});
