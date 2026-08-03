import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { distPath } from "../helpers.mjs";

const { snapshots } = await import(distPath("index.js"));
const { DEFAULT_REGISTRY_URL, resolveSnapshot, resolveRegistryUrl } = snapshots;

const CHANNEL = "https://registry.vpod.sh/v1/nextjs/snapshots.json";

describe("resolveRegistryUrl", () => {
    it("defaults to the public registry", () => {
        delete process.env.VPOD_REGISTRY;
        assert.equal(resolveRegistryUrl(undefined), DEFAULT_REGISTRY_URL);
    });

    it("takes an explicit url, which is how a partner reaches a channel", () => {
        assert.equal(resolveRegistryUrl(CHANNEL), CHANNEL);
    });

    it("falls back to VPOD_REGISTRY", () => {
        process.env.VPOD_REGISTRY = CHANNEL;
        try {
            assert.equal(resolveRegistryUrl(undefined), CHANNEL);
        } finally {
            delete process.env.VPOD_REGISTRY;
        }
    });

    it("lets an explicit url win over the environment", () => {
        process.env.VPOD_REGISTRY = CHANNEL;
        try {
            assert.equal(resolveRegistryUrl(DEFAULT_REGISTRY_URL), DEFAULT_REGISTRY_URL);
        } finally {
            delete process.env.VPOD_REGISTRY;
        }
    });

    it("treats an empty string as unset", () => {
        delete process.env.VPOD_REGISTRY;
        assert.equal(resolveRegistryUrl(""), DEFAULT_REGISTRY_URL);
    });
});

describe("resolveSnapshot error", () => {
    const listed = [{ id: "vsnap-base-256mb", name: "vsnap-base", tag: "1.0.0" }];
    const escaped = DEFAULT_REGISTRY_URL.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

    it("names the registry it searched", () => {
        assert.throws(
            () => resolveSnapshot(listed, "vsnap-nextjs", DEFAULT_REGISTRY_URL),
            new RegExp(`not found in ${escaped}`),
        );
    });

    it("still lists what is available", () => {
        assert.throws(
            () => resolveSnapshot(listed, "nope", DEFAULT_REGISTRY_URL),
            /vsnap-base:1\.0\.0/,
        );
    });

    it("omits the registry when it was not given", () => {
        assert.throws(() => resolveSnapshot(listed, "nope"), /not found\. Available/);
    });

    it("says 'nothing' rather than trailing off on an empty catalogue", () => {
        assert.throws(
            () => resolveSnapshot([], "nope", DEFAULT_REGISTRY_URL),
            /Available: nothing/,
        );
    });
});
