import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { distPath } from "../helpers.mjs";

const { CodeExecution, CommandResult, parseCodeOutput } = await import(distPath("index.js"));

describe("CommandResult", () => {
    it("treats exit code zero as success", () => {
        assert.equal(new CommandResult("out").success, true);
        assert.equal(new CommandResult("out", "", 0).success, true);
    });

    it("treats any nonzero exit code as failure", () => {
        assert.equal(new CommandResult("", "boom", 1).success, false);
        assert.equal(new CommandResult("", "", 124).success, false);
    });

    it("defaults stderr to empty and exit code to zero", () => {
        const result = new CommandResult("only stdout");
        assert.equal(result.stderr, "");
        assert.equal(result.exitCode, 0);
    });
});

describe("CodeExecution", () => {
    it("is successful when there is no error", () => {
        assert.equal(new CodeExecution("6").success, true);
    });

    it("is unsuccessful when an error is set", () => {
        assert.equal(new CodeExecution("", "ValueError: bad").success, false);
    });
});

describe("parseCodeOutput", () => {
    it("returns the trimmed text and one log line per output line", () => {
        const execution = parseCodeOutput("first\nsecond\n");
        assert.equal(execution.text, "first\nsecond");
        assert.deepEqual(execution.logs, ["first", "second"]);
        assert.equal(execution.success, true);
    });

    it("produces no logs for empty output", () => {
        const execution = parseCodeOutput("   \n");
        assert.equal(execution.text, "");
        assert.deepEqual(execution.logs, []);
    });

    it("detects a traceback as an error", () => {
        const execution = parseCodeOutput("Traceback (most recent call last):\nValueError: nope");
        assert.equal(execution.success, false);
        assert.equal(execution.error, "ValueError: nope");
    });

    it("keeps the last error when several lines match", () => {
        const execution = parseCodeOutput("KeyError: a\nValueError: b");
        assert.equal(execution.error, "ValueError: b");
    });

    it("finds errors that only appear on stderr", () => {
        const execution = parseCodeOutput("fine", "sh: bogus: not found");
        assert.equal(execution.success, false);
        assert.equal(execution.error, "sh: bogus: not found");
        assert.deepEqual(execution.logs, ["fine"], "logs come from stdout only");
    });

    it("does not treat ordinary output as an error", () => {
        const execution = parseCodeOutput("errors are handled gracefully here");
        assert.equal(execution.success, true, "lowercase 'errors' is not a marker");
    });

    it("absorbs the CRLF line endings the guest REPL emits", () => {
        const execution = parseCodeOutput("one\r\ntwo\r\n");
        assert.deepEqual(execution.logs, ["one", "two"]);
        assert.equal(execution.text, "one\ntwo");
    });

    it("absorbs a lone carriage return", () => {
        assert.deepEqual(parseCodeOutput("one\rtwo").logs, ["one", "two"]);
    });
});
