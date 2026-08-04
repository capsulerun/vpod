/**
 *`wasi:sockets` backend whose connections are served by `fetch` running on another thread.
 */

import { streams } from "../shims/io.js";
import { registerSynchronousReadiness } from "../shims/pollable.js";
import { poll } from "@bytecodealliance/preview2-shim/io";
import { RING_FAILED, RingReader, createRing } from "./ring.js";
import { SyntheticAddresses } from "./synthetic-dns.js";
import type {
    AddressResolution,
    IpAddressFamily,
    IpSocketAddress,
    ShutdownType,
    SocketBackend,
    TcpConnection,
    UdpConnection,
} from "../shims/sockets.js";
import type { DriverCommand } from "./driver-protocol.js";

const streamClasses = streams as unknown as {
    InputStream: { new (handler?: object): object };
    OutputStream: { new (handler?: object): object };
};
const InputStreamClass = streamClasses.InputStream;
const OutputStreamClass = streamClasses.OutputStream;
const PollableClass = poll.Pollable as unknown as { new (settled?: Promise<unknown>): object };

function neverSettling(): Promise<never> {
    return new Promise<never>(() => {});
}

const disposeSymbol: symbol =
    (Symbol as { dispose?: symbol }).dispose ?? Symbol.for("dispose");

function disposesQuietly(prototype: object): void {
    Object.defineProperty(prototype, disposeSymbol, {
        value(): void {},
        writable: true,
        configurable: true,
    });
}

class ReadinessPollable extends PollableClass {
    readonly #isReady: () => boolean;

    constructor(isReady: () => boolean) {
        super(neverSettling());
        this.#isReady = isReady;
        registerSynchronousReadiness(this, { isReady, readyAtMilliseconds: null });
    }

    ready(): boolean {
        return this.#isReady();
    }
}

class RingInputStream extends InputStreamClass {
    readonly #reader: RingReader;

    constructor(reader: RingReader) {
        super({});
        this.#reader = reader;
    }

    read(length: bigint | number): Uint8Array {
        if (this.#reader.finished()) {
            throw this.#reader.state() === RING_FAILED
                ? { tag: "last-operation-failed", val: new Error("vpod: request failed") }
                : { tag: "closed" };
        }

        return this.#reader.read(Number(length));
    }

    blockingRead(length: bigint | number): Uint8Array {
        for (;;) {
            const chunk = this.read(length);
            if (chunk.length > 0) {
                return chunk;
            }
        }
    }

    skip(length: bigint | number): bigint {
        return BigInt(this.read(length).length);
    }

    blockingSkip(length: bigint | number): bigint {
        return BigInt(this.blockingRead(length).length);
    }

    subscribe(): object {
        return new ReadinessPollable(
            () => this.#reader.available() > 0 || this.#reader.finished(),
        );
    }
}

class DriverOutputStream extends OutputStreamClass {
    readonly #send: (bytes: Uint8Array) => void;

    constructor(send: (bytes: Uint8Array) => void) {
        super({});
        this.#send = send;
    }

    checkWrite(): bigint {
        return 1n << 20n;
    }

    write(contents: Uint8Array): void {
        this.#send(contents.slice());
    }

    blockingWriteAndFlush(contents: Uint8Array): void {
        this.write(contents);
    }

    flush(): void {}

    blockingFlush(): void {}

    writeZeroes(length: bigint): void {
        this.write(new Uint8Array(Number(length)));
    }

    blockingWriteZeroesAndFlush(length: bigint): void {
        this.writeZeroes(length);
    }

    subscribe(): object {
        return new ReadinessPollable(() => true);
    }
}

disposesQuietly(ReadinessPollable.prototype);
disposesQuietly(RingInputStream.prototype);
disposesQuietly(DriverOutputStream.prototype);

function ipv4Of(address: IpSocketAddress): { port: number; address: number[] } | undefined {
    if (address.tag !== "ipv4") {
        return undefined;
    }
    return address.val as { port: number; address: number[] };
}

class FetchTcpConnection implements TcpConnection {
    readonly #id: number;
    readonly #post: (command: DriverCommand, transfer?: Transferable[]) => void;
    readonly #addresses: SyntheticAddresses;
    readonly #allowedPorts: ReadonlySet<number>;

    #reader: RingReader | undefined;
    #remote: IpSocketAddress | undefined;
    #opened = false;
    #disposed = false;

    constructor(
        id: number,
        post: (command: DriverCommand, transfer?: Transferable[]) => void,
        addresses: SyntheticAddresses,
        allowedPorts: ReadonlySet<number>,
    ) {
        this.#id = id;
        this.#post = post;
        this.#addresses = addresses;
        this.#allowedPorts = allowedPorts;
    }

    startConnect(remoteAddress: IpSocketAddress): void {
        const ipv4 = ipv4Of(remoteAddress);
        if (ipv4 === undefined) {
            throw "address-family-not-supported";
        }

        if (!this.#allowedPorts.has(ipv4.port)) {
            throw "access-denied";
        }

        this.#remote = remoteAddress;

        const ring = createRing();
        this.#reader = new RingReader(ring);
        this.#opened = true;

        this.#post({
            kind: "open",
            id: this.#id,
            ring,
            resolvedHostname: this.#addresses.hostnameFor(ipv4.address),
            port: ipv4.port,
        });
    }

    finishConnect(): [object, object] {
        if (!this.#opened || this.#reader === undefined) {
            throw "not-in-progress";
        }

        return [
            new RingInputStream(this.#reader),
            new DriverOutputStream((bytes) => {
                this.#post({ kind: "send", id: this.#id, bytes: bytes.buffer as ArrayBuffer }, [
                    bytes.buffer as ArrayBuffer,
                ]);
            }),
        ];
    }

    subscribe(): object {
        return new ReadinessPollable(() => this.#opened);
    }

    localAddress(): IpSocketAddress {
        return { tag: "ipv4", val: { port: 0, address: [10, 0, 2, 15] } };
    }

    remoteAddress(): IpSocketAddress {
        if (this.#remote === undefined) {
            throw "invalid-state";
        }
        return this.#remote;
    }

    shutdown(kind: ShutdownType): void {
        if (kind === "receive") {
            return;
        }
        this.#post({ kind: "shutdown", id: this.#id });
    }

    dispose(): void {
        if (this.#disposed) {
            return;
        }
        this.#disposed = true;
        this.#post({ kind: "close", id: this.#id });
    }
}

class SyntheticResolution implements AddressResolution {
    #remaining: number[][];

    constructor(addresses: number[][]) {
        this.#remaining = addresses;
    }

    resolveNextAddress(): unknown {
        const next = this.#remaining.shift();
        if (next === undefined) {
            return undefined;
        }
        return { tag: "ipv4", val: next };
    }

    subscribe(): object {
        return new ReadinessPollable(() => true);
    }

    dispose(): void {
        this.#remaining = [];
    }
}

export interface FetchBackendOptions {
    allowedPorts?: number[];
}

export class FetchSocketBackend implements SocketBackend {
    readonly name = "fetch";

    readonly #post: (command: DriverCommand, transfer?: Transferable[]) => void;
    readonly #addresses = new SyntheticAddresses();
    readonly #allowedPorts: Set<number>;

    #nextConnectionId = 1;

    constructor(
        post: (command: DriverCommand, transfer?: Transferable[]) => void,
        options: FetchBackendOptions = {},
    ) {
        this.#post = post;
        this.#allowedPorts = new Set(options.allowedPorts ?? [80, 443]);
    }

    createTcpConnection(family: IpAddressFamily): TcpConnection {
        if (family !== "ipv4") {
            throw "address-family-not-supported";
        }

        return new FetchTcpConnection(
            this.#nextConnectionId++,
            this.#post,
            this.#addresses,
            this.#allowedPorts,
        );
    }

    createUdpConnection(): UdpConnection {
        throw "access-denied";
    }

    resolveAddresses(hostname: string): AddressResolution {
        const literal = hostname.split(".").map(Number);
        if (literal.length === 4 && literal.every((part) => Number.isInteger(part) && part >= 0 && part <= 255)) {
            return new SyntheticResolution([literal]);
        }

        return new SyntheticResolution([Array.from(this.#addresses.addressFor(hostname))]);
    }

}
