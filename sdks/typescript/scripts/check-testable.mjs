#!/usr/bin/env node

import { closeSync, existsSync, openSync, readSync, statSync } from "node:fs";

import { locateSnapshot, skipReason } from "../tests/helpers.mjs";

const LZ4_MAGIC = [0x04, 0x22, 0x4d, 0x18];

function fail(message) {
    console.error(`vpod: ${message}`);
    process.exit(1);
}

const reason = skipReason();
if (reason !== null) {
    fail(`integration tests would skip: ${reason}`);
}

const requested = process.env.VPOD_TEST_SNAPSHOT;
const chosen = locateSnapshot();

if (requested !== undefined && requested !== "") {
    if (!existsSync(requested)) {
        fail(`VPOD_TEST_SNAPSHOT is set to ${requested}, which does not exist`);
    }
    if (chosen !== requested) {
        fail(
            `VPOD_TEST_SNAPSHOT is ${requested} but ${chosen} was chosen instead, ` +
                `so the tests would not exercise the intended snapshot`,
        );
    }
}

const { size } = statSync(chosen);
if (size < 1024 * 1024) {
    fail(`${chosen} is only ${size} bytes, which is far too small to be a snapshot`);
}

// Catches an error page or a truncated download. It cannot tell a snapshot
// framed twice from one framed once, because that needs an actual lz4 decode;
// the workflow does that check with the lz4 CLI instead.
const header = Buffer.alloc(4);
const file = openSync(chosen, "r");
try {
    readSync(file, header, 0, 4, 0);
} finally {
    closeSync(file);
}

const isLz4 = LZ4_MAGIC.every((byte, index) => header[index] === byte);
const isVpod = header.toString("latin1") === "VPOD";

if (!isLz4 && !isVpod) {
    fail(
        `${chosen} starts with ${[...header].map((b) => b.toString(16).padStart(2, "0")).join(" ")}, ` +
            `which is neither lz4 nor VPOD`,
    );
}

console.log(
    `integration tests are runnable: ${chosen} (${(size / 1048576).toFixed(1)} MiB, ${isLz4 ? "lz4" : "raw VPOD"})`,
);
