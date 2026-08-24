import assert from "node:assert/strict";
import http from "node:http";
import { after, describe, it } from "node:test";

import { distPath } from "../helpers.mjs";

const { snapshots } = await import(distPath("index.js"));
const {
    authHeaders,
    checkApiKeyKind,
    isBrowser,
    PRIVATE_REGISTRY_URL,
    PUBLIC_REGISTRY_URL,
    pullSnapshot,
    resolveApiKey,
    resolveRegistryUrl,
    resolveSnapshot,
    SnapshotAuthError,
} = snapshots;

/** Node has no `document`, so the browser-only branches are the "outside" ones. */
function withEnv(vars, run) {
    const saved = {};
    for (const [key, value] of Object.entries(vars)) {
        saved[key] = process.env[key];
        if (value === undefined) delete process.env[key];
        else process.env[key] = value;
    }
    try {
        return run();
    } finally {
        for (const [key, value] of Object.entries(saved)) {
            if (value === undefined) delete process.env[key];
            else process.env[key] = value;
        }
    }
}

describe("registry precedence", () => {
    it("is one chain: explicit, then env, then key-or-not", () => {
        withEnv({ VPOD_REGISTRY: undefined }, () => {
            assert.equal(resolveRegistryUrl(undefined, undefined), PUBLIC_REGISTRY_URL);
            assert.equal(resolveRegistryUrl(undefined, "vpod_sk_k"), PRIVATE_REGISTRY_URL);
            assert.equal(resolveRegistryUrl("https://e.co/c.json", "vpod_sk_k"), "https://e.co/c.json");
        });
    });

    it("lets VPOD_REGISTRY beat the key default", () => {
        withEnv({ VPOD_REGISTRY: "https://self.hosted/c.json" }, () => {
            assert.equal(resolveRegistryUrl(undefined, "vpod_sk_k"), "https://self.hosted/c.json");
        });
    });

    it("prefers an explicit key over VPOD_API_KEY", () => {
        withEnv({ VPOD_API_KEY: "vpod_sk_from_env" }, () => {
            assert.equal(resolveApiKey("vpod_sk_explicit"), "vpod_sk_explicit");
            assert.equal(resolveApiKey(undefined), "vpod_sk_from_env");
        });
    });
});

describe("key kinds", () => {
    it("knows this is not a browser", () => {
        assert.equal(isBrowser(), false);
    });

    it("refuses a publishable key where no Origin can be checked", () => {
        assert.throws(() => checkApiKeyKind("vpod_pk_abc"), /no browser here/);
    });

    it("accepts a secret key outside a browser", () => {
        checkApiKeyKind("vpod_sk_abc");
    });

    it("refuses an unrecognised prefix rather than guessing", () => {
        assert.throws(() => checkApiKeyKind("sk-openai-style"), /vpod_sk_/);
    });
});

describe("the key never leaves the registry origin", () => {
    const registry = "https://api.vpod.sh/v1/snapshots.json";
    const key = "vpod_sk_secret";

    it("attaches the header on the registry's own origin", () => {
        assert.equal(
            authHeaders("https://api.vpod.sh/v1/blob/x", registry, key).Authorization,
            `Bearer ${key}`,
        );
    });

    it("attaches nothing anywhere else", () => {
        for (const hostile of [
            "https://attacker.com/blob",
            "http://api.vpod.sh/v1/blob/x",
            "https://api.vpod.sh.attacker.com/x",
            "https://api.vpod.sh:8443/v1/blob/x",
        ]) {
            assert.deepEqual(authHeaders(hostile, registry, key), {}, hostile);
        }
    });

    it("attaches nothing when there is no key", () => {
        assert.deepEqual(authHeaders("https://api.vpod.sh/x", registry, undefined), {});
    });
});

describe("not-found says whether a key was sent", () => {
    const catalogue = [{ id: "vsnap-1", name: "other", tag: "1.0" }];

    it("says so when it did not", () => {
        assert.throws(
            () => resolveSnapshot(catalogue, "missing", "https://r/c.json", false),
            /No API key was sent/,
        );
    });

    it("says so when it did", () => {
        assert.throws(
            () => resolveSnapshot(catalogue, "missing", "https://r/c.json", true),
            /An API key WAS sent/,
        );
    });
});

it("still resolves the default snapshot when a key is present", () => {
    // The whole reason the catalogue is ONE request rather than two.
    const orgCatalogue = [
        { id: "vsnap-base-256mb", name: "vsnap-base", tag: "1.0.0" },
        { id: "vsnap-private", name: "mine", tag: "1.0" },
    ];
    assert.equal(
        resolveSnapshot(orgCatalogue, "vsnap-base:latest", undefined, true).id,
        "vsnap-base-256mb",
    );
});

// Module scope, not inside `describe`: a describe callback is synchronous, so
// `await server.listen(...)` inside one is a SyntaxError rather than a wait.
const payload = new TextEncoder().encode("VPODtest-snapshot-bytes");
let catalogueHits = 0;
let blobHits = 0;
let refuseUntil = 0;
let seenBlobAuth = [];

const { createHash } = await import("node:crypto");

const server = http.createServer((request, response) => {
    const host = request.headers.host;
    if (request.url.startsWith("/catalogue")) {
        catalogueHits += 1;
        response.writeHead(200, { "content-type": "application/json" });
        response.end(
            JSON.stringify({
                version: "1",
                snapshots: [
                    {
                        id: "vsnap-priv",
                        name: "mine",
                        tag: "1.0",
                        memory_label: "256MB",
                        description: "",
                        url: `http://${host}/blob/vsnap-priv`,
                        sha256: createHash("sha256").update(payload).digest("hex"),
                        size: payload.byteLength,
                    },
                ],
            }),
        );
        return;
    }
    blobHits += 1;
    seenBlobAuth.push(request.headers.authorization ?? null);
    if (blobHits <= refuseUntil) {
        response.writeHead(403);
        response.end();
        return;
    }
    response.writeHead(200, { "content-length": String(payload.byteLength) });
    response.end(Buffer.from(payload));
});

await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const registryUrl = `http://127.0.0.1:${server.address().port}/catalogue`;
after(() => server.close());

const reset = (refuse) => {
    catalogueHits = 0;
    blobHits = 0;
    refuseUntil = refuse;
    seenBlobAuth = [];
};

describe("expired signed URL", () => {
    it("refreshes the catalogue once and retries once", async () => {
        reset(1);
        const pulled = await pullSnapshot({
            name: "mine:1.0", registryUrl, apiKey: "vpod_sk_k", store: null,
        });
        assert.equal(new TextDecoder().decode(pulled.bytes), "VPODtest-snapshot-bytes");
        assert.equal(blobHits, 2, "expected exactly one retry");
        assert.equal(catalogueHits, 2, "expected exactly one forced refresh");
    });

    it("fails readably on a second refusal instead of looping", async () => {
        reset(99);
        await assert.rejects(
            pullSnapshot({ name: "mine:1.0", registryUrl, apiKey: "vpod_sk_k", store: null }),
            (error) =>
                error instanceof SnapshotAuthError &&
                /refused again after refreshing/.test(error.message),
        );
        assert.equal(blobHits, 2, "must stop after one retry, not loop");
    });

    it("sends the key to a same-origin blob", async () => {
        reset(0);
        await pullSnapshot({
            name: "mine:1.0", registryUrl, apiKey: "vpod_sk_k", store: null,
        });
        assert.deepEqual(seenBlobAuth, ["Bearer vpod_sk_k"]);
    });

    it("sends no header when there is no key", async () => {
        reset(0);
        await withEnv({ VPOD_API_KEY: undefined }, () =>
            pullSnapshot({ name: "mine:1.0", registryUrl, store: null }));
        assert.deepEqual(seenBlobAuth, [null]);
    });
});
