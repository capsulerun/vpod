import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { distPath } from "../helpers.mjs";

const { ipNameLookup } = await import(distPath("node/host-resolver.js"));
const { sockets } = await import("@bytecodealliance/preview2-shim");
const network = sockets.instanceNetwork.instanceNetwork();

function resolveAll(name) {
    const stream = ipNameLookup.resolveAddresses(network, name);
    stream.subscribe().block();

    const addresses = [];
    for (;;) {
        const next = stream.resolveNextAddress();
        if (next === undefined) {
            return addresses;
        }
        addresses.push(next);
    }
}

describe("the host resolver the Node target provides", () => {
    it("answers an ip literal without going near a lookup", () => {
        assert.deepEqual(resolveAll("127.0.0.1"), [{ tag: "ipv4", val: [127, 0, 0, 1] }]);
    });

    it("resolves a name the OS knows", () => {
        assert.ok(
            resolveAll("localhost").length > 0,
            "the host resolver returned no addresses, so every guest lookup is " +
                "silently falling through to slirp's hardcoded upstream DNS",
        );
    });

    it("returns the variant shape jco lowers, not the array the shim wraps it in", () => {
        const addresses = resolveAll("localhost");
        assert.ok(addresses.length > 0, "nothing to check the shape of");

        for (const address of addresses) {
            assert.ok(
                !Array.isArray(address),
                `expected a variant, got an array: ${JSON.stringify(address)}`,
            );
            assert.match(address.tag, /^ipv[46]$/);
            assert.ok(Array.isArray(address.val));
            assert.equal(address.val.length, address.tag === "ipv4" ? 4 : 8);
        }
    });
});
