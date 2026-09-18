import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { distPath } from "../helpers.mjs";

const { Sandbox, Trace } = await import(distPath("index.js"));

const { cases } = JSON.parse(
    readFileSync(
        resolve(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "trace-summaries.json"),
        "utf8",
    ),
);

const camelCase = (key) => key.replace(/_([a-z])/g, (_, letter) => letter.toUpperCase());

function withCamelCaseKeys(value) {
    if (Array.isArray(value)) return value.map(withCamelCaseKeys);
    if (value !== null && typeof value === "object") {
        return Object.fromEntries(
            Object.entries(value).map(([key, inner]) => [camelCase(key), withCamelCaseKeys(inner)]),
        );
    }
    return value;
}

describe("trace summaries match the fixture the Python SDK checks", () => {
    for (const fixture of cases) {
        it(fixture.name, () => {
            const trace = new Trace(fixture.events);

            assert.deepEqual(trace.files(), withCamelCaseKeys(fixture.files));
            assert.deepEqual(
                trace.files({ internal: true, noise: true }),
                withCamelCaseKeys(fixture.files_including_internal_and_noise),
            );
            assert.deepEqual(trace.network(), withCamelCaseKeys(fixture.network));
            assert.deepEqual(
                trace.network({ internal: true }),
                withCamelCaseKeys(fixture.network_including_internal),
            );
            assert.deepEqual(trace.processes(), withCamelCaseKeys(fixture.processes));
            assert.deepEqual(
                trace.processes({ internal: true }),
                withCamelCaseKeys(fixture.processes_including_internal),
            );
            assert.equal(trace.complete, fixture.complete);
        });

        it(`${fixture.name}: JSON lines carry every event`, () => {
            const lines = new Trace(fixture.events).toJSONL().split("\n").filter(Boolean);
            assert.deepEqual(lines.map((line) => JSON.parse(line)), fixture.events);
        });
    }

    it("keeps its own copy of the events", () => {
        const events = [{ v: 1, seq: 0, guest_ns: 0, wall_ms: 0, kind: "trace.dropped", count: 1 }];
        const trace = new Trace(events);
        events.length = 0;

        assert.equal(trace.events.length, 1);
        assert.throws(() => trace.events.push({}), TypeError);
    });
});

describe("trace options", () => {
    it("refuses an unknown source before anything starts", async () => {
        await assert.rejects(Sandbox.create({ trace: { netwrok: true } }), /netwrok/);
    });
});
