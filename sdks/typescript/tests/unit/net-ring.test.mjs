import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { distPath } from "../helpers.mjs";

const { RingReader, RingWriter, createRing, RING_OPEN, RING_ENDED, RING_FAILED } = await import(
    distPath("net/ring.js")
);

const bytes = (text) => new TextEncoder().encode(text);
const text = (buffer) => new TextDecoder().decode(buffer);

describe("shared ring", () => {
    it("carries bytes from writer to reader", () => {
        const ring = createRing(64);
        const writer = new RingWriter(ring);
        const reader = new RingReader(ring);

        assert.equal(reader.available(), 0);
        assert.equal(writer.write(bytes("hello")), 5);
        assert.equal(reader.available(), 5);
        assert.equal(text(reader.read(10)), "hello");
        assert.equal(reader.available(), 0);
    });

    it("reads no more than asked, leaving the rest", () => {
        const ring = createRing(64);
        const writer = new RingWriter(ring);
        const reader = new RingReader(ring);

        writer.write(bytes("abcdef"));
        assert.equal(text(reader.read(2)), "ab");
        assert.equal(text(reader.read(2)), "cd");
        assert.equal(text(reader.read(99)), "ef");
    });

    it("wraps around the end of the buffer without losing or reordering bytes", () => {
        const capacity = 16;
        const ring = createRing(capacity);
        const writer = new RingWriter(ring);
        const reader = new RingReader(ring);

        let sent = "";
        let received = "";
        for (let round = 0; round < 200; round++) {
            const piece = `${round % 10}`.repeat(5);
            const written = writer.write(bytes(piece));
            sent += piece.slice(0, written);
            received += text(reader.read(7));
        }
        received += text(reader.read(capacity));

        assert.equal(received, sent);
        assert.ok(sent.length > capacity * 10, `only moved ${sent.length} bytes`);
    });

    it("writes only what fits and says so", () => {
        const ring = createRing(8);
        const writer = new RingWriter(ring);

        assert.equal(writer.write(bytes("0123456789")), 8);
        assert.equal(writer.write(bytes("more")), 0);
        assert.equal(writer.freeSpace(), 0);
    });

    it("keeps buffered bytes readable after the writer ends", () => {
        const ring = createRing(64);
        const writer = new RingWriter(ring);
        const reader = new RingReader(ring);

        writer.write(bytes("tail"));
        writer.end();

        assert.equal(reader.state(), RING_ENDED);
        assert.equal(reader.finished(), false, "buffered bytes must still be delivered");
        assert.equal(text(reader.read(64)), "tail");
        assert.equal(reader.finished(), true);
    });

    it("reports failure distinctly from a clean end", () => {
        const ring = createRing(64);
        const writer = new RingWriter(ring);
        const reader = new RingReader(ring);

        assert.equal(reader.state(), RING_OPEN);
        writer.fail();
        assert.equal(reader.state(), RING_FAILED);
        assert.equal(reader.finished(), true);
    });

    it("frees space for the writer as the reader drains", async () => {
        const ring = createRing(8);
        const writer = new RingWriter(ring);
        const reader = new RingReader(ring);

        writer.write(bytes("01234567"));
        assert.equal(writer.freeSpace(), 0);

        const waiting = writer.waitForSpace(2000);
        reader.read(4);

        assert.equal(await waiting, true);
        assert.equal(writer.freeSpace(), 4);
    });

    it("gives up waiting rather than hanging when nothing drains", async () => {
        const ring = createRing(8);
        const writer = new RingWriter(ring);

        writer.write(bytes("01234567"));
        assert.equal(await writer.waitForSpace(120), false);
    });

    it("refuses a capacity that is not a power of two", () => {
        assert.throws(() => createRing(100), /power of two/);
    });
});
