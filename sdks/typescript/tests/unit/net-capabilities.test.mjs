import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { distPath } from "../helpers.mjs";

const { capabilitiesOf, explainUnreachable } = await import(distPath("net/capabilities.js"));

describe("network capabilities", () => {
    it("reports no network by default, which is what an offline sandbox has", () => {
        const none = capabilitiesOf("none");

        assert.equal(none.backend, "none");
        assert.equal(none.rawTcp, false);
        assert.equal(none.udp, false);
    });

    it("describes fetch as CORS-bound, HTTP-only and not byte-faithful", () => {
        const fetchBackend = capabilitiesOf("fetch");

        assert.equal(fetchBackend.corsRestricted, true);
        assert.equal(fetchBackend.rawTcp, false);
        assert.equal(fetchBackend.arbitraryPorts, false);
        assert.equal(fetchBackend.byteFaithfulHeaders, false);
        assert.ok(fetchBackend.strippedRequestHeaders.includes("host"));
        assert.ok(fetchBackend.strippedRequestHeaders.includes("content-length"));
    });

    it("describes real sockets as unrestricted, which is Node's case", () => {
        const sockets = capabilitiesOf("sockets");

        assert.equal(sockets.corsRestricted, false);
        assert.equal(sockets.rawTcp, true);
        assert.equal(sockets.arbitraryPorts, true);
        assert.equal(sockets.byteFaithfulHeaders, true);
        assert.equal(sockets.udp, true);
        assert.deepEqual(sockets.strippedRequestHeaders, []);
    });

    it("the two backends differ where the transports genuinely differ", () => {
        const fetchBackend = capabilitiesOf("fetch");
        const sockets = capabilitiesOf("sockets");

        // Pinned so a future backend cannot quietly claim parity it lacks.
        assert.notEqual(fetchBackend.rawTcp, sockets.rawTcp);
        assert.notEqual(fetchBackend.corsRestricted, sockets.corsRestricted);
        assert.notEqual(fetchBackend.byteFaithfulHeaders, sockets.byteFaithfulHeaders);
    });

    it("hands back an owned array, so a caller cannot mutate the table", () => {
        const first = capabilitiesOf("fetch");
        first.strippedRequestHeaders.push("invented");

        assert.ok(!capabilitiesOf("fetch").strippedRequestHeaders.includes("invented"));
    });

    it("explains an unreachable port instead of letting it fail obscurely", () => {
        assert.equal(explainUnreachable(capabilitiesOf("fetch"), 443), null);
        assert.equal(explainUnreachable(capabilitiesOf("fetch"), 80), null);
        assert.match(explainUnreachable(capabilitiesOf("fetch"), 6379), /port 6379/);
        assert.equal(explainUnreachable(capabilitiesOf("sockets"), 6379), null);
        assert.match(explainUnreachable(capabilitiesOf("none"), 443), /no network/);
    });
});
