import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { distPath } from "../helpers.mjs";

const { parsePreamble } = await import(distPath("net/preamble.js"));
const { parseRequest, serializeResponse, toFetchable, serializeTransportError } = await import(
    distPath("net/http-codec.js")
);
const { SyntheticAddresses } = await import(distPath("net/synthetic-dns.js"));

const bytes = (text) => new TextEncoder().encode(text);
const text = (buffer) => new TextDecoder().decode(buffer);

describe("preamble", () => {
    it("reads the host and port the emulator announced", () => {
        const result = parsePreamble(bytes("VPOD-CONNECT pypi.org 443\nGET / HTTP/1.1\r\n\r\n"));

        assert.equal(result.kind, "ok");
        assert.equal(result.preamble.host, "pypi.org");
        assert.equal(result.preamble.port, 443);
        assert.equal(result.preamble.consumed, "VPOD-CONNECT pypi.org 443\n".length);
    });

    it("waits rather than guessing when the line is still arriving", () => {
        assert.equal(parsePreamble(bytes("VPOD-CONN")).kind, "incomplete");
        assert.equal(parsePreamble(bytes("VPOD-CONNECT pypi.org 4")).kind, "incomplete");
    });

    it("gives up once a line could no longer be a preamble", () => {
        const flood = parsePreamble(bytes("x".repeat(300)));
        assert.equal(flood.kind, "invalid");

        assert.equal(parsePreamble(bytes("GET / HTTP/1.1\r\n")).kind, "invalid");
        assert.equal(parsePreamble(bytes("VPOD-CONNECT pypi.org\n")).kind, "invalid");
        assert.equal(parsePreamble(bytes("VPOD-CONNECT pypi.org 0\n")).kind, "invalid");
        assert.equal(parsePreamble(bytes("VPOD-CONNECT pypi.org 70000\n")).kind, "invalid");
        assert.equal(parsePreamble(bytes("VPOD-CONNECT  443\n")).kind, "invalid");
    });
});

describe("request parsing", () => {
    it("reads a request with no body", () => {
        const result = parseRequest(bytes("GET /simple/ HTTP/1.1\r\nHost: pypi.org\r\n\r\n"));

        assert.equal(result.kind, "ok");
        assert.equal(result.request.method, "GET");
        assert.equal(result.request.target, "/simple/");
        assert.deepEqual(result.request.headers, [["Host", "pypi.org"]]);
        assert.equal(result.request.body, undefined);
        assert.equal(result.request.keepAlive, true);
    });

    it("waits for a body the Content-Length promised", () => {
        const head = "POST /u HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\n\r\n";

        assert.equal(parseRequest(bytes(head)).kind, "incomplete");
        assert.equal(parseRequest(bytes(`${head}abc`)).kind, "incomplete");

        const complete = parseRequest(bytes(`${head}abcde`));
        assert.equal(complete.kind, "ok");
        assert.equal(text(complete.request.body), "abcde");
    });

    it("reassembles a chunked body", () => {
        const wire = "POST /u HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n3\r\n hi\r\n0\r\n\r\n";
        const result = parseRequest(bytes(wire));

        assert.equal(result.kind, "ok");
        assert.equal(text(result.request.body), "hello hi");
        assert.equal(result.request.consumed, bytes(wire).length);
    });

    it("reports how much it consumed so the next request can be read", () => {
        const first = "GET /a HTTP/1.1\r\nHost: h\r\n\r\n";
        const second = "GET /b HTTP/1.1\r\nHost: h\r\n\r\n";
        const stream = bytes(first + second);

        const one = parseRequest(stream);
        assert.equal(one.kind, "ok");
        assert.equal(one.request.consumed, bytes(first).length);

        const two = parseRequest(stream.slice(one.request.consumed));
        assert.equal(two.kind, "ok");
        assert.equal(two.request.target, "/b");
    });

    it("treats Connection: close as the end of the connection", () => {
        const result = parseRequest(
            bytes("GET / HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n"),
        );

        assert.equal(result.kind, "ok");
        assert.equal(result.request.keepAlive, false);
    });

    it("defaults HTTP/1.0 to closing, which is what the guest's wget sends", () => {
        const result = parseRequest(bytes("GET / HTTP/1.0\r\n\r\n"));

        assert.equal(result.kind, "ok");
        assert.equal(result.request.keepAlive, false);
    });
});

describe("turning a request into a fetch", () => {
    it("takes the origin from the connection, not the request line", () => {
        const parsed = parseRequest(bytes("GET /simple/flask/ HTTP/1.1\r\nHost: evil\r\n\r\n"));
        const fetchable = toFetchable(parsed.request, "pypi.org", 443, true);

        assert.equal(fetchable.url, "https://pypi.org/simple/flask/");
    });

    it("takes the scheme from whether the guest wanted TLS, not from the port", () => {
        const parsed = parseRequest(bytes("GET /x HTTP/1.1\r\n\r\n"));

        assert.equal(toFetchable(parsed.request, "h", 80, false).url, "http://h/x");
        assert.equal(toFetchable(parsed.request, "h", 443, true).url, "https://h/x");

        assert.equal(toFetchable(parsed.request, "h", 8080, false).url, "http://h:8080/x");
        assert.equal(toFetchable(parsed.request, "h", 8443, true).url, "https://h:8443/x");
    });

    it("strips the headers the browser insists on generating itself", () => {
        const parsed = parseRequest(
            bytes(
                "GET / HTTP/1.1\r\nHost: h\r\nConnection: keep-alive\r\n" +
                    "User-Agent: pip/24\r\nAccept-Encoding: gzip\r\nSec-Fetch-Mode: cors\r\n\r\n",
            ),
        );
        const fetchable = toFetchable(parsed.request, "h", 443, true);

        assert.deepEqual(fetchable.headers, [["User-Agent", "pip/24"]]);
        assert.deepEqual(fetchable.stripped.sort(), [
            "Accept-Encoding",
            "Connection",
            "Host",
            "Sec-Fetch-Mode",
        ]);
    });
});

describe("response serialization", () => {
    it("always states a Content-Length and never chunks", () => {
        const wire = text(
            serializeResponse(200, "OK", [["Content-Type", "application/json"]], bytes("{}"), true),
        );

        assert.match(wire, /^HTTP\/1\.1 200 OK\r\n/);
        assert.match(wire, /\r\nContent-Type: application\/json\r\n/);
        assert.match(wire, /\r\nContent-Length: 2\r\n/);
        assert.match(wire, /\r\nConnection: keep-alive\r\n/);
        assert.ok(wire.endsWith("\r\n\r\n{}"));
    });

    it("drops framing headers that describe a body we already decoded", () => {
        const wire = text(
            serializeResponse(
                200,
                "OK",
                [
                    ["Content-Encoding", "gzip"],
                    ["Transfer-Encoding", "chunked"],
                    ["Content-Length", "999"],
                ],
                bytes("plain"),
                false,
            ),
        );

        assert.doesNotMatch(wire, /Content-Encoding/);
        assert.doesNotMatch(wire, /Transfer-Encoding/);
        assert.match(wire, /\r\nContent-Length: 5\r\n/);
        assert.match(wire, /\r\nConnection: close\r\n/);
    });

    it("turns a transport failure into a status the guest's client will surface", () => {
        const wire = text(serializeTransportError("pypi.org refused the preflight"));

        assert.match(wire, /^HTTP\/1\.1 502 Bad Gateway\r\n/);
        assert.match(wire, /pypi\.org refused the preflight/);
    });
});

describe("synthetic addresses", () => {
    it("is stable per hostname and distinct between hostnames", () => {
        const addresses = new SyntheticAddresses();

        const first = addresses.addressFor("pypi.org");
        assert.deepEqual(Array.from(addresses.addressFor("pypi.org")), Array.from(first));
        assert.notDeepEqual(
            Array.from(addresses.addressFor("files.pythonhosted.org")),
            Array.from(first),
        );
    });

    it("stays inside the benchmarking range, clear of the guest's own subnet", () => {
        const addresses = new SyntheticAddresses();
        const address = addresses.addressFor("example.com");

        assert.equal(address[0], 198);
        assert.ok(address[1] === 18 || address[1] === 19);
    });

    it("maps an address back to the name it was minted for", () => {
        const addresses = new SyntheticAddresses();
        const address = addresses.addressFor("registry.npmjs.org");

        assert.equal(addresses.hostnameFor(address), "registry.npmjs.org");
        assert.equal(addresses.hostnameFor([10, 0, 2, 2]), undefined);
    });
});
