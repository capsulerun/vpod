import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { FetchDriver } from "../../dist/net/fetch-driver.js";
import { createRing } from "../../dist/net/ring.js";

async function withFetch(handler, body) {
    const original = globalThis.fetch;
    globalThis.fetch = handler;
    try {
        return await body();
    } finally {
        globalThis.fetch = original;
    }
}

async function asBrowser(body) {
    const had = "window" in globalThis;
    if (!had) globalThis.window = {};
    try {
        return await body();
    } finally {
        if (!had) delete globalThis.window;
    }
}

const REQUEST =
    "VPOD-CONNECT dl-cdn.alpinelinux.org 443\n" +
    "GET /alpine/x HTTP/1.1\r\nHost: dl-cdn.alpinelinux.org\r\n\r\n";

async function exchange(driver, id = 1) {
    driver.handle({
        kind: "open",
        id,
        ring: createRing(),
        resolvedHostname: undefined,
        port: 443,
    });
    driver.handle({ kind: "send", id, bytes: new TextEncoder().encode(REQUEST).buffer });
    driver.handle({ kind: "shutdown", id });
    await new Promise((done) => setTimeout(done, 20));
}

const PROXY = "https://browser-proxy.vpod.sh";
const DIRECT = "https://dl-cdn.alpinelinux.org/alpine/x";

describe("corsProxy", () => {
    it("goes direct when the destination allows it", async () => {
        const seen = [];
        await asBrowser(() =>
            withFetch(
                async (url) => {
                    seen.push(url);
                    return new Response("ok", { status: 200 });
                },
                async () => {
                    await exchange(new FetchDriver({ corsProxy: PROXY }));
                },
            ),
        );

        assert.deepEqual(seen, [DIRECT], "a working host must not touch the proxy");
    });

    it("retries through the proxy when the browser refuses the direct fetch", async () => {
        const seen = [];
        await asBrowser(() =>
            withFetch(
                async (url) => {
                    seen.push(url);
                    if (!url.startsWith(PROXY)) throw new TypeError("Failed to fetch");
                    return new Response("ok", { status: 200 });
                },
                async () => {
                    await exchange(new FetchDriver({ corsProxy: PROXY }));
                },
            ),
        );

        assert.deepEqual(seen, [DIRECT, `${PROXY}/${DIRECT}`]);
    });

    it("remembers the host, so only the first request pays a failure", async () => {
        const seen = [];
        await asBrowser(() =>
            withFetch(
                async (url) => {
                    seen.push(url);
                    if (!url.startsWith(PROXY)) throw new TypeError("Failed to fetch");
                    return new Response("ok", { status: 200 });
                },
                async () => {
                    const driver = new FetchDriver({ corsProxy: PROXY });
                    await exchange(driver, 1);
                    await exchange(driver, 2);
                    await exchange(driver, 3);
                },
            ),
        );

        assert.deepEqual(seen, [
            DIRECT,
            `${PROXY}/${DIRECT}`,
            `${PROXY}/${DIRECT}`,
            `${PROXY}/${DIRECT}`,
        ]);
    });

    it("does not retry when no proxy is configured", async () => {
        const seen = [];
        await asBrowser(() =>
            withFetch(
                async (url) => {
                    seen.push(url);
                    throw new TypeError("Failed to fetch");
                },
                async () => {
                    await exchange(new FetchDriver());
                },
            ),
        );

        assert.deepEqual(seen, [DIRECT], "without corsProxy the failure must stand");
    });

    it("tolerates a trailing slash on the proxy URL", async () => {
        const seen = [];
        await asBrowser(() =>
            withFetch(
                async (url) => {
                    seen.push(url);
                    if (!url.startsWith(PROXY)) throw new TypeError("Failed to fetch");
                    return new Response("ok", { status: 200 });
                },
                async () => {
                    await exchange(new FetchDriver({ corsProxy: `${PROXY}/` }));
                },
            ),
        );

        assert.equal(seen[1], `${PROXY}/${DIRECT}`, "no doubled slash");
    });
});
