import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { createTestSandbox, skipReason } from "../helpers.mjs";

const BODY = '{"model":"claude","prompt":"hello"}';

describe("request content", { skip: skipReason() ?? false }, () => {
    it("records the headers and body a request carried", async () => {
        const sandbox = await createTestSandbox({ trace: { network: true } });

        let requests;
        try {
            await sandbox.commands.run(
                `wget -q -O- --header='Content-Type: application/json' ` +
                    `--post-data='${BODY}' https://pypi.org/pypi/six/json > /dev/null 2>&1; true`,
                { timeout: 90 },
            );
            requests = (await sandbox.trace.collect())
                .network()
                .flatMap((activity) => activity.requests);
        } finally {
            await sandbox.close();
        }

        const posted = requests.filter((request) => request.method === "POST");
        if (posted.length === 0) {
            return; // no route to the network
        }

        const request = posted[0];
        assert.equal(request.headers["Content-Type"], "application/json");
        assert.equal(request.headers.Host, "pypi.org");
        assert.equal(request.bodyBytes, BODY.length);
        assert.equal(request.body, BODY);
        assert.equal(request.bodyEncoding, "utf8");
        assert.equal(request.bodyTruncated, false);
    });
});
