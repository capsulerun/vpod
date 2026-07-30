import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { distPath } from "../helpers.mjs";

const filesystem = await import(distPath("shims/filesystem.js"));
const io = await import(distPath("shims/io.js"));
const clocks = await import(distPath("shims/clocks.js"));

describe("filesystem shim", () => {
    it("mounts a snapshot under the snap directory and reads it back", () => {
        const bytes = new Uint8Array([1, 2, 3, 4]);
        const path = filesystem.mountSnapshot("vsnap-base-256mb.snap", bytes);

        assert.equal(path, "snap/vsnap-base-256mb.snap");
        assert.deepEqual(filesystem.readGuestFile(path), bytes);
    });

    it("mounts a delta under its own directory", () => {
        const path = filesystem.mountDelta("one.bin", new Uint8Array([9]));
        assert.equal(path, "deltas/one.bin");
        assert.equal(filesystem.deltaPath("one.bin"), path);
    });

    it("returns null for a path that is absent", () => {
        assert.equal(filesystem.readGuestFile("snap/missing.snap"), null);
        assert.equal(filesystem.readGuestFile("nowhere/at/all"), null);
    });

    it("returns null for a directory rather than pretending it is a file", () => {
        assert.equal(filesystem.readGuestFile("snap"), null);
    });

    it("removes a file", () => {
        const path = filesystem.mountDelta("two.bin", new Uint8Array([7]));
        filesystem.removeGuestFile(path);
        assert.equal(filesystem.readGuestFile(path), null);
    });

    it("keeps snapshots and deltas mounted at the same time", () => {
        filesystem.mountSnapshot("both-256mb.snap", new Uint8Array([1]));
        filesystem.mountDelta("both.bin", new Uint8Array([2]));

        assert.notEqual(filesystem.readGuestFile("snap/both-256mb.snap"), null);
        assert.notEqual(filesystem.readGuestFile("deltas/both.bin"), null);
    });
});

describe("poll shim", () => {
    it("reports a timer that has already elapsed without waiting", () => {
        const pollable = clocks.monotonicClock.subscribeDuration(0n);
        const startedAt = performance.now();
        const ready = io.poll.poll([pollable]);

        assert.deepEqual([...ready], [0]);
        assert.ok(performance.now() - startedAt < 50, "an elapsed timer must not spin");
    });

    it("waits for the deadline and then reports ready", () => {
        const pollable = clocks.monotonicClock.subscribeDuration(20n * 1_000_000n);
        const startedAt = performance.now();
        const ready = io.poll.poll([pollable]);
        const waited = performance.now() - startedAt;

        assert.deepEqual([...ready], [0]);
        assert.ok(waited >= 15, `expected to wait about 20ms, waited ${waited.toFixed(1)}ms`);
    });

    it("returns the earliest of several timers", () => {
        const soon = clocks.monotonicClock.subscribeDuration(5n * 1_000_000n);
        const later = clocks.monotonicClock.subscribeDuration(10_000n * 1_000_000n);
        const startedAt = performance.now();
        const ready = io.poll.poll([later, soon]);

        assert.ok([...ready].includes(1), "the 5ms timer should be reported ready");
        assert.ok(performance.now() - startedAt < 1000, "must not wait for the 10s timer");
    });

    it("counts its spins so the cost stays measurable", () => {
        io.resetPollStats();
        io.poll.poll([clocks.monotonicClock.subscribeDuration(5n * 1_000_000n)]);

        const stats = io.pollStats();
        assert.equal(stats.spinCount, 1);
        assert.ok(stats.spinNanoseconds > 0);
    });

    it("rejects an empty list", () => {
        assert.throws(() => io.poll.poll([]), /must not be empty/);
    });

    it("refuses to block on a pollable with no synchronous readiness", () => {
        const unready = { ready: () => false };
        assert.throws(() => io.poll.poll([unready]), /no .*synchronous readiness source/s);
    });

    it("hands back the vendor Pollable class the component checks against", () => {
        const pollable = clocks.monotonicClock.subscribeDuration(0n);
        assert.ok(
            pollable instanceof io.poll.Pollable,
            "the component asserts `ret instanceof Pollable` on every timer",
        );
    });
});

describe("sockets shim", () => {
    it("starts offline", async () => {
        const { socketBackendName } = await import(distPath("shims/sockets.js"));
        assert.equal(socketBackendName(), "offline");
    });

    it("exposes every class the component destructures at load time", async () => {
        const sockets = await import(distPath("shims/sockets.js"));

        assert.equal(typeof sockets.tcp.TcpSocket, "function");
        assert.equal(typeof sockets.udp.UdpSocket, "function");
        assert.equal(typeof sockets.udp.IncomingDatagramStream, "function");
        assert.equal(typeof sockets.udp.OutgoingDatagramStream, "function");
        assert.equal(typeof sockets.ipNameLookup.ResolveAddressStream, "function");
        assert.equal(typeof sockets.network.Network, "function");
        assert.equal(typeof sockets.tcpCreateSocket.createTcpSocket, "function");
        assert.equal(typeof sockets.udpCreateSocket.createUdpSocket, "function");
        assert.equal(typeof sockets.instanceNetwork.instanceNetwork, "function");
    });

    it("constructs the instance network without failing", async () => {
        const sockets = await import(distPath("shims/sockets.js"));
        assert.doesNotThrow(() => sockets.instanceNetwork.instanceNetwork());
    });

    it("refuses a connection with a real wasi error code", async () => {
        const sockets = await import(distPath("shims/sockets.js"));

        assert.throws(() => sockets.tcpCreateSocket.createTcpSocket("ipv4"), (thrown) => {
            assert.equal(thrown, "access-denied");
            return true;
        });
    });

    it("refuses name resolution with a resolver error code", async () => {
        const sockets = await import(distPath("shims/sockets.js"));

        assert.throws(
            () => sockets.ipNameLookup.resolveAddresses({}, "example.com"),
            (thrown) => {
                assert.equal(thrown, "permanent-resolver-failure");
                return true;
            },
        );
    });
});
