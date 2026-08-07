import assert from "node:assert/strict";
import { afterEach, describe, it } from "node:test";

import { distPath } from "../helpers.mjs";

const { FetchDriver } = await import(distPath("net/fetch-driver.js"));
const { RingReader, createRing } = await import(distPath("net/ring.js"));

const bytes = (text) => new TextEncoder().encode(text);
const decode = (buffer) => new TextDecoder().decode(buffer);

function connect(driver, { id = 1, capacity = 1 << 16, resolvedHostname, port = 443 } = {}) {
    const ring = createRing(capacity);
    const reader = new RingReader(ring);

    driver.handle({ kind: "open", id, ring, resolvedHostname, port });

    return {
        send(text) {
            driver.handle({ kind: "send", id, bytes: bytes(text).buffer });
        },
        async drainUntilFinished(timeoutMilliseconds = 2000) {
            const deadline = Date.now() + timeoutMilliseconds;
            let collected = "";
            while (Date.now() < deadline) {
                collected += decode(reader.read(capacity));
                if (reader.finished()) break;
                await new Promise((resolve) => setTimeout(resolve, 1));
            }
            return collected + decode(reader.read(capacity));
        },
    };
}

function stubFetch(handler) {
    globalThis.fetch = async (url, init) => handler(url, init);
}

/** The driver only blames CORS where CORS exists, which under Node it does not. */
function pretendToBeABrowser() {
    globalThis.WorkerGlobalScope = class WorkerGlobalScope {};
}

function pretendThePageIsSecure() {
    globalThis.location = { protocol: "https:" };
}

function collectWarnings() {
    const warnings = [];
    const original = console.warn;
    console.warn = (message) => warnings.push(String(message));
    return {
        warnings,
        restore() {
            console.warn = original;
        },
    };
}

async function refuse(driver, host, thrown) {
    stubFetch(() => {
        throw thrown;
    });
    const connection = connect(driver);
    connection.send(`VPOD-CONNECT ${host} 443\n`);
    connection.send("GET / HTTP/1.1\r\nHost: h\r\n\r\n");
    return connection.drainUntilFinished();
}

async function refusePlaintext(driver, host, thrown) {
    stubFetch(() => {
        throw thrown;
    });
    const connection = connect(driver, { resolvedHostname: host, port: 80 });
    connection.send("GET / HTTP/1.1\r\nHost: h\r\n\r\n");
    return connection.drainUntilFinished();
}

afterEach(() => {
    delete globalThis.WorkerGlobalScope;
    delete globalThis.location;
});

describe("browser fetch failures", () => {
    it("names CORS as the likely cause, so a 502 is not mistaken for a dead host", async () => {
        pretendToBeABrowser();
        const captured = collectWarnings();

        try {
            const wire = await refuse(
                new FetchDriver(),
                "dl-cdn.alpinelinux.org",
                new TypeError("Failed to fetch"),
            );

            // reason phrase has to carry the point on its own.
            assert.match(wire, /^HTTP\/1\.1 502 Blocked by browser CORS policy/);
            assert.match(wire, /access-control-allow-origin/);
            assert.match(wire, /dl-cdn\.alpinelinux\.org/);
            // The original error has to survive; it is the only ground truth.
            assert.match(wire, /Failed to fetch/);
            // Node is the way out, and saying so saves a support round trip.
            assert.match(wire, /Node/);
        } finally {
            captured.restore();
        }
    });

    it("calls plaintext from an https page mixed content rather than CORS", async () => {
        pretendToBeABrowser();
        pretendThePageIsSecure();
        const captured = collectWarnings();

        try {
            const wire = await refusePlaintext(
                new FetchDriver(),
                "browser.vpod.sh",
                new TypeError("Failed to fetch"),
            );

            assert.match(wire, /^HTTP\/1\.1 502 Blocked as mixed content/);
            assert.doesNotMatch(wire, /access-control-allow-origin/);
            assert.match(wire, /https:\/\/browser\.vpod\.sh/);
            assert.match(wire, /Failed to fetch/);
        } finally {
            captured.restore();
        }
    });

    it("keeps blaming CORS for plaintext from an http page, where nothing is mixed", async () => {
        pretendToBeABrowser();
        globalThis.location = { protocol: "http:" };
        const captured = collectWarnings();

        try {
            const wire = await refusePlaintext(
                new FetchDriver(),
                "example.com",
                new TypeError("Failed to fetch"),
            );

            assert.match(wire, /^HTTP\/1\.1 502 Blocked by browser CORS policy/);
            assert.match(wire, /access-control-allow-origin/);
        } finally {
            captured.restore();
        }
    });

    it("warns on the host side, where a developer will actually see it", async () => {
        pretendToBeABrowser();
        const captured = collectWarnings();

        try {
            const driver = new FetchDriver();
            await refuse(driver, "example.com", new TypeError("Failed to fetch"));

            assert.equal(captured.warnings.length, 1);
            assert.match(captured.warnings[0], /example\.com/);
            assert.match(captured.warnings[0], /access-control-allow-origin/);
        } finally {
            captured.restore();
        }
    });

    it("warns once per host rather than once per retry", async () => {
        pretendToBeABrowser();
        const captured = collectWarnings();

        try {
            const driver = new FetchDriver();
            for (let attempt = 0; attempt < 3; attempt++) {
                await refuse(driver, "example.com", new TypeError("Failed to fetch"));
            }
            await refuse(driver, "other.example", new TypeError("Failed to fetch"));

            assert.equal(captured.warnings.length, 2);
        } finally {
            captured.restore();
        }
    });

    it("stays quiet about CORS under Node, where the failure is a real one", async () => {
        const captured = collectWarnings();

        try {
            const wire = await refuse(
                new FetchDriver(),
                "unreachable.invalid",
                new TypeError("fetch failed"),
            );

            assert.match(wire, /^HTTP\/1\.1 502 Bad Gateway/);
            assert.match(wire, /fetch failed/);
            assert.doesNotMatch(wire, /access-control-allow-origin/);
        } finally {
            captured.restore();
        }
    });

    it("calls a timeout a timeout instead of blaming the host", async () => {
        pretendToBeABrowser();
        const captured = collectWarnings();

        try {
            const abort = new Error("The operation was aborted.");
            abort.name = "AbortError";

            const wire = await refuse(
                new FetchDriver({ requestTimeoutMilliseconds: 5000 }),
                "slow.example",
                abort,
            );

            assert.match(wire, /^HTTP\/1\.1 502 Upstream Timeout/);
            assert.match(wire, /timed out after 5s/);
            assert.doesNotMatch(wire, /access-control-allow-origin/);
        } finally {
            captured.restore();
        }
    });
});
