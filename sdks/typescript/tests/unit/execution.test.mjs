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

    it("reports the exception line when the runner exits non-zero", () => {
        const execution = parseCodeOutput(
            "Traceback (most recent call last):\nValueError: nope",
            "",
            1,
        );
        assert.equal(execution.success, false);
        assert.equal(execution.error, "ValueError: nope");
    });

    it("prefers stderr for the message when there is one", () => {
        const execution = parseCodeOutput("fine", "sh: bogus: not found", 127);
        assert.equal(execution.success, false);
        assert.equal(execution.error, "sh: bogus: not found");
        assert.deepEqual(execution.logs, ["fine"], "logs come from stdout only");
    });

    it("keeps stderr reachable on a run that succeeded", () => {
        const execution = parseCodeOutput("out\r\n", "careful\r\n", 0);
        assert.equal(execution.success, true);
        assert.equal(execution.text, "out");
        assert.equal(execution.stderr, "careful");
        assert.deepEqual(execution.logs, ["out"]);
    });

    it("still says something when a failing run printed nothing", () => {
        const execution = parseCodeOutput("", "", 3);
        assert.equal(execution.success, false);
        assert.equal(execution.error, "exited 3");
    });

    it("does not quote ordinary output as the reason a run failed", () => {
        const execution = parseCodeOutput("saving to disk\ndone\n", "", 3);
        assert.equal(execution.success, false);
        assert.equal(execution.error, "exited 3");
        assert.deepEqual(execution.logs, ["saving to disk", "done"]);
    });

    it("quotes the exception when stdout carries a traceback", () => {
        const execution = parseCodeOutput(
            'before\nTraceback (most recent call last):\n  File "<vpod>", line 1\nValueError: boom',
            "",
            1,
        );
        assert.equal(execution.error, "ValueError: boom");
    });

    it("normalises the carriage returns the guest sends", () => {
        const execution = parseCodeOutput("a\r\nb\r\n");
        assert.equal(execution.text, "a\nb");
        assert.deepEqual(execution.logs, ["a", "b"]);
    });


    it("trusts the exit code over anything the program printed", () => {
        for (const output of [
            "Error handling is hard",
            "Traceback analysis complete",
            "0 errors, 0 warnings",
            "sh: bogus: not found",
            '{"error": null}',
        ]) {
            const execution = parseCodeOutput(output, "", 0);
            assert.equal(execution.success, true, `treated as a failure: ${output}`);
            assert.equal(execution.error, null);
        }
    });

    it("absorbs the CRLF line endings the guest REPL emits", () => {
        const execution = parseCodeOutput("one\r\ntwo\r\n");
        assert.deepEqual(execution.logs, ["one", "two"]);
        assert.equal(execution.text, "one\ntwo");
    });

    // This used to assert the opposite, from the SDK's first commit and never
    // revisited. A lone \r is a program redrawing its line, not a line ending:
    // splitting on it reported lines the guest never printed.
    it("keeps a lone carriage return, which redraws a line rather than ending one", () => {
        assert.deepEqual(parseCodeOutput("one\rtwo").logs, ["one\rtwo"]);
        assert.deepEqual(parseCodeOutput("50%\r100%\r\ndone\r\n").logs, ["50%\r100%", "done"]);
    });
});
