import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { distPath } from "../helpers.mjs";

const { FetchDriver } = await import(distPath("net/fetch-driver.js"));
const { RingReader, createRing } = await import(distPath("net/ring.js"));

const bytes = (text) => new TextEncoder().encode(text);
const decode = (buffer) => new TextDecoder().decode(buffer);

function connect(driver, id = 1, capacity = 1 << 16, open = {}) {
    const ring = createRing(capacity);
    const reader = new RingReader(ring);

    driver.handle({
        kind: "open",
        id,
        ring,
        resolvedHostname: undefined,
        port: 443,
        ...open,
    });

    return {
        send(text) {
            const copy = bytes(text);
            driver.handle({ kind: "send", id, bytes: copy.buffer });
        },
        async drainUntilFinished(timeoutMilliseconds = 2000) {
            const deadline = Date.now() + timeoutMilliseconds;
            let collected = "";

            while (Date.now() < deadline) {
                collected += decode(reader.read(capacity));
                if (reader.finished()) {
                    break;
                }
                await new Promise((resolve) => setTimeout(resolve, 1));
            }

            return collected + decode(reader.read(capacity));
        },
        async drainFor(milliseconds) {
            const deadline = Date.now() + milliseconds;
            let collected = "";
            while (Date.now() < deadline) {
                collected += decode(reader.read(capacity));
                await new Promise((resolve) => setTimeout(resolve, 1));
            }
            return collected + decode(reader.read(capacity));
        },
    };
}

function stubFetch(handler) {
    const calls = [];
    globalThis.fetch = async (url, init) => {
        calls.push({ url, init });
        return handler(url, init);
    };
    return calls;
}

function jsonResponse(body, status = 200) {
    return new Response(body, {
        status,
        headers: { "Content-Type": "application/json" },
    });
}

describe("fetch driver", () => {
    it("dials the host from the preamble, not the request's Host header", async () => {
        const calls = stubFetch(() => jsonResponse("{}"));
        const driver = new FetchDriver();
        const connection = connect(driver);

        connection.send("VPOD-CONNECT pypi.org 443\n");
        connection.send("GET /simple/flask/ HTTP/1.1\r\nHost: attacker.invalid\r\n\r\n");

        const wire = await connection.drainUntilFinished();

        assert.equal(calls.length, 1);
        assert.equal(calls[0].url, "https://pypi.org/simple/flask/");
        assert.match(wire, /^HTTP\/1\.1 200 OK\r\n/);
        assert.ok(wire.endsWith("{}"));
    });

    it("tolerates the preamble and request arriving in one write or many", async () => {
        const calls = stubFetch(() => jsonResponse("ok"));
        const driver = new FetchDriver();
        const connection = connect(driver);

        for (const character of "VPOD-CONNECT pypi.org 443\nGET / HTTP/1.1\r\n\r\n") {
            connection.send(character);
        }

        await connection.drainUntilFinished();
        assert.equal(calls.length, 1);
        assert.equal(calls[0].url, "https://pypi.org/");
    });

    it("serves a raw plaintext connection off the hostname synthetic DNS resolved", async () => {
        const calls = stubFetch(() => jsonResponse("ok"));
        const driver = new FetchDriver();
        const connection = connect(driver, 1, 1 << 16, {
            resolvedHostname: "example.com",
            port: 80,
        });

        connection.send("GET /x HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n");

        const wire = await connection.drainUntilFinished();

        assert.equal(calls.length, 1);
        assert.equal(calls[0].url, "http://example.com/x");
        assert.match(wire, /^HTTP\/1\.1 200/);
    });

    it("still refuses a connection with neither a preamble nor a resolved hostname", async () => {
        stubFetch(() => jsonResponse("ok"));
        const driver = new FetchDriver();
        const connection = connect(driver, 1, 1 << 16, { port: 80 });

        connection.send("GET /x HTTP/1.1\r\nHost: example.com\r\n\r\n");

        const wire = await connection.drainUntilFinished();

        assert.match(wire, /^HTTP\/1\.1 502/);
        assert.match(wire, /VPOD-CONNECT/);
    });

    it("answers two keep-alive requests in order on one connection", async () => {
        const bodies = { "/a": "first", "/b": "second" };
        stubFetch((url) => jsonResponse(bodies[new URL(url).pathname]));

        const driver = new FetchDriver();
        const connection = connect(driver);

        connection.send("VPOD-CONNECT pypi.org 443\n");
        connection.send(
            "GET /a HTTP/1.1\r\nHost: h\r\n\r\nGET /b HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
        );

        const wire = await connection.drainUntilFinished();

        assert.ok(
            wire.indexOf("first") < wire.indexOf("second"),
            `responses came back out of order: ${wire}`,
        );
    });

    it("carries a request body through to fetch", async () => {
        const calls = stubFetch(() => jsonResponse("stored"));
        const driver = new FetchDriver();
        const connection = connect(driver);

        connection.send("VPOD-CONNECT example.com 443\n");
        connection.send(
            "POST /upload HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
        );

        await connection.drainUntilFinished();

        assert.equal(calls[0].init.method, "POST");
        assert.equal(decode(calls[0].init.body), "hello");
    });

    it("turns a fetch rejection into a 502 the guest can read", async () => {
        stubFetch(() => {
            throw new TypeError("Failed to fetch");
        });

        const driver = new FetchDriver();
        const connection = connect(driver);

        connection.send("VPOD-CONNECT dl-cdn.alpinelinux.org 443\n");
        connection.send("GET / HTTP/1.1\r\nHost: h\r\n\r\n");

        const wire = await connection.drainUntilFinished();

        assert.match(wire, /^HTTP\/1\.1 502 Bad Gateway/);
        assert.match(wire, /Failed to fetch/);
    });

    it("dials an absolute-form target at its own authority", async () => {
        const calls = stubFetch(() => jsonResponse("ok"));
        const driver = new FetchDriver();
        const connection = connect(driver);

        connection.send("VPOD-CONNECT pypi.org 443\n");
        connection.send("GET https://files.pythonhosted.org/x HTTP/1.1\r\nHost: pypi.org\r\n\r\n");

        const wire = await connection.drainUntilFinished();

        assert.equal(calls.length, 1);
        assert.equal(calls[0].url, "https://files.pythonhosted.org/x");
        assert.match(wire, /^HTTP\/1\.1 200/);
    });

    it("rejects a connection that is not speaking the preamble", async () => {
        const calls = stubFetch(() => jsonResponse("no"));
        const driver = new FetchDriver();
        const connection = connect(driver);

        connection.send("GET / HTTP/1.1\r\nHost: h\r\n\r\n");

        const wire = await connection.drainUntilFinished();

        assert.equal(calls.length, 0);
        assert.match(wire, /502 Bad Gateway/);
    });

    it("waits for a partial request rather than sending half of it", async () => {
        const calls = stubFetch(() => jsonResponse("done"));
        const driver = new FetchDriver();
        const connection = connect(driver);

        connection.send("VPOD-CONNECT pypi.org 443\n");
        connection.send("POST /u HTTP/1.1\r\nHost: h\r\nContent-Length: 10\r\n\r\nfour");

        assert.equal(await connection.drainFor(60), "");
        assert.equal(calls.length, 0);

        connection.send("56789");
        assert.equal(await connection.drainFor(60), "");
        assert.equal(calls.length, 0, "a body one byte short is still not a request");

        connection.send("!");
        await connection.drainFor(120);
        assert.equal(calls.length, 1);
    });

    it("delivers a body larger than the ring, a wheel being the real case", async () => {
        const wheel = "w".repeat(300_000);
        stubFetch(() => new Response(wheel));

        const driver = new FetchDriver();
        const connection = connect(driver, 1, 1 << 12);

        connection.send("VPOD-CONNECT files.pythonhosted.org 443\n");
        connection.send("GET /flask.whl HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n");

        const wire = await connection.drainUntilFinished(15_000);
        const body = wire.slice(wire.indexOf("\r\n\r\n") + 4);

        assert.match(wire, /Content-Length: 300000\r\n/);
        assert.equal(body.length, wheel.length);
        assert.equal(body, wheel);
    });
});
