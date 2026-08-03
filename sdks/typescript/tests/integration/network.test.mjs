import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { basename } from "node:path";
import { after, before, describe, it } from "node:test";

import { distPath, loadSdk, locateSnapshot, skipReason } from "../helpers.mjs";

const reason = skipReason();

const { setSocketBackend } = await loadSdk();
const { FetchSocketBackend } = await import(distPath("net/fetch-backend.js"));
const { RingWriter } = await import(distPath("net/ring.js"));
const { parsePreamble } = await import(distPath("net/preamble.js"));
const { parseRequest, serializeResponse } = await import(distPath("net/http-codec.js"));
const cli = await import(distPath("shims/cli.js"));

const encoder = new TextEncoder();

function concat(left, right) {
    const joined = new Uint8Array(left.length + right.length);
    joined.set(left, 0);
    joined.set(right, left.length);
    return joined;
}

function cannedUpstream(respond) {
    const seen = [];
    const connections = new Map();

    const advance = (connection) => {
        if (connection.host === null) {
            const preamble = parsePreamble(connection.buffered);
            if (preamble.kind !== "ok") {
                return;
            }
            connection.host = preamble.preamble.host;
            connection.port = preamble.preamble.port;
            connection.buffered = connection.buffered.slice(preamble.preamble.consumed);
        }

        const parsed = parseRequest(connection.buffered);
        if (parsed.kind !== "ok") {
            return;
        }
        connection.buffered = connection.buffered.slice(parsed.request.consumed);

        seen.push({
            host: connection.host,
            port: connection.port,
            request: parsed.request,
        });

        const answer = respond(connection.host, parsed.request);
        const body = encoder.encode(answer.body);
        const wire = serializeResponse(
            answer.status,
            "",
            [["Content-Type", "text/plain"]],
            body,
            false,
        );

        assert.ok(
            connection.writer.write(wire) === wire.length,
            "canned response did not fit the ring",
        );
        connection.writer.end();
    };

    const backend = new FetchSocketBackend((command) => {
        if (command.kind === "open") {
            connections.set(command.id, {
                writer: new RingWriter(command.ring),
                buffered: new Uint8Array(0),
                host: null,
                port: 0,
            });
            return;
        }

        const connection = connections.get(command.id);
        if (connection === undefined) {
            return;
        }

        if (command.kind === "send") {
            connection.buffered = concat(connection.buffered, new Uint8Array(command.bytes));
            advance(connection);
        }
    });

    return { backend, seen };
}

describe("network transport, end to end through the emulator", { skip: reason ?? false }, () => {
    let sandbox;
    let seen;
    let respond = (host) => ({ status: 200, body: `hello from ${host}\n` });

    before(async () => {
        cli._setEnv({ VPOD_HOST_TLS: "1" });

        const { Sandbox, createInlineTransport } = await loadSdk();
        const upstream = cannedUpstream((host, request) => respond(host, request));
        seen = upstream.seen;
        setSocketBackend(upstream.backend);

        const snapshotPath = locateSnapshot();
        sandbox = await Sandbox.create({
            transport: await createInlineTransport(),
            snapshot: {
                bytes: readFileSync(snapshotPath),
                name: basename(snapshotPath),
            },
        });
    });

    after(async () => {
        await sandbox?.close();
    });

    it("resolves a hostname to a synthetic address", async () => {
        const result = await sandbox.commands.run("getent hosts example.test", {
            timeout: 30,
        });

        assert.match(result.stdout, /198\.18\./);
        assert.match(result.stdout, /example\.test/);
    });

    it("carries an https request out and the reply back", async () => {
        const result = await sandbox.commands.run("wget -q -O- https://example.test/hi", {
            timeout: 60,
        });

        assert.equal(result.exitCode, 0, `wget failed: ${result.stderr}`);
        assert.match(result.stdout, /hello from example\.test/);
    });

    it("tells the transport which host to dial, taken from the guest's own preamble", () => {
        const last = seen.at(-1);

        assert.equal(last.host, "example.test");
        assert.equal(last.port, 443);
        assert.equal(last.request.target, "/hi");
    });

    it("closes the connection, so a pipe reading it sees end-of-input", async () => {
        const result = await sandbox.commands.run(
            "wget -q -O- https://example.test/piped | head -c 200; echo rc=$?",
            { timeout: 45 },
        );

        assert.match(result.stdout, /hello from example\.test/);
        assert.match(
            result.stdout,
            /rc=0/,
            "the pipeline never finished, so no FIN reached the guest",
        );
    });

    it("refuses a port the transport cannot serve, rather than failing later", () => {
        const backend = new FetchSocketBackend(() => {}, { allowedPorts: [443] });
        const connection = backend.createTcpConnection("ipv4");

        assert.throws(
            () =>
                connection.startConnect({
                    tag: "ipv4",
                    val: { port: 6379, address: [1, 2, 3, 4] },
                }),
            /access-denied/,
        );

        assert.doesNotThrow(() =>
            backend.createTcpConnection("ipv4").startConnect({
                tag: "ipv4",
                val: { port: 443, address: [1, 2, 3, 4] },
            }),
        );
    });

    it("closes a refused connection, which the guest never reads the body of", async () => {
        respond = () => ({
            status: 502,
            body: "vpod: refused by the transport\n",
        });

        const result = await sandbox.commands.run(
            "wget -q -O- https://example.test/refused | head -c 200; echo rc=$?",
            { timeout: 45 },
        );

        assert.match(
            result.stdout,
            /rc=/,
            "the pipeline never finished: a refused connection is never closed",
        );
    });
});
