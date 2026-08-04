/**
 * single-producer single-consumer byte ring in a `SharedArrayBuffer`.
 */

const CONTROL_INTS = 4;
const CONTROL_BYTES = CONTROL_INTS * 4;

const WRITE_CURSOR = 0;
const READ_CURSOR = 1;
const STATE = 2;

export const RING_OPEN = 0;
export const RING_ENDED = 1;
export const RING_FAILED = 2;

const DEFAULT_RING_CAPACITY = 1 << 20;

const POLL_INTERVAL_MS = 5;

export function createRing(capacity = DEFAULT_RING_CAPACITY): SharedArrayBuffer {
    if ((capacity & (capacity - 1)) !== 0) {
        throw new Error(`ring capacity must be a power of two, got ${capacity}`);
    }
    return new SharedArrayBuffer(CONTROL_BYTES + capacity);
}

function distance(ahead: number, behind: number): number {
    return (ahead - behind) | 0;
}

export class RingWriter {
    readonly #control: Int32Array;
    readonly #data: Uint8Array;
    readonly #capacity: number;

    constructor(buffer: SharedArrayBuffer) {
        this.#control = new Int32Array(buffer, 0, CONTROL_INTS);
        this.#data = new Uint8Array(buffer, CONTROL_BYTES);
        this.#capacity = this.#data.length;
    }

    get capacity(): number {
        return this.#capacity;
    }

    freeSpace(): number {
        const write = Atomics.load(this.#control, WRITE_CURSOR);
        const read = Atomics.load(this.#control, READ_CURSOR);
        return this.#capacity - distance(write, read);
    }

    write(bytes: Uint8Array): number {
        const writable = Math.min(bytes.length, this.freeSpace());
        if (writable === 0) {
            return 0;
        }

        const write = Atomics.load(this.#control, WRITE_CURSOR);
        const start = write & (this.#capacity - 1);
        const untilEnd = Math.min(writable, this.#capacity - start);

        this.#data.set(bytes.subarray(0, untilEnd), start);
        if (writable > untilEnd) {
            this.#data.set(bytes.subarray(untilEnd, writable), 0);
        }

        Atomics.store(this.#control, WRITE_CURSOR, (write + writable) | 0);
        Atomics.notify(this.#control, WRITE_CURSOR);

        return writable;
    }

    end(): void {
        Atomics.store(this.#control, STATE, RING_ENDED);
        Atomics.notify(this.#control, WRITE_CURSOR);
    }

    fail(): void {
        Atomics.store(this.#control, STATE, RING_FAILED);
        Atomics.notify(this.#control, WRITE_CURSOR);
    }

    async waitForSpace(timeoutMilliseconds = 30_000): Promise<boolean> {
        const deadline = Date.now() + timeoutMilliseconds;

        while (this.freeSpace() === 0) {
            if (Date.now() > deadline) {
                return false;
            }

            const read = Atomics.load(this.#control, READ_CURSOR);
            const waiter = (
                Atomics as unknown as {
                    waitAsync?: (
                        typedArray: Int32Array,
                        index: number,
                        value: number,
                        timeout: number,
                    ) => { async: boolean; value: Promise<string> | string };
                }
            ).waitAsync;

            const tick = new Promise<void>((resolve) => setTimeout(resolve, POLL_INTERVAL_MS));
            const woken = waiter?.(this.#control, READ_CURSOR, read, POLL_INTERVAL_MS);

            await (woken?.async === true ? Promise.race([woken.value, tick]) : tick);
        }

        return true;
    }
}

export class RingReader {
    readonly #control: Int32Array;
    readonly #data: Uint8Array;
    readonly #capacity: number;

    constructor(buffer: SharedArrayBuffer) {
        this.#control = new Int32Array(buffer, 0, CONTROL_INTS);
        this.#data = new Uint8Array(buffer, CONTROL_BYTES);
        this.#capacity = this.#data.length;
    }

    available(): number {
        const write = Atomics.load(this.#control, WRITE_CURSOR);
        const read = Atomics.load(this.#control, READ_CURSOR);
        return distance(write, read);
    }

    state(): number {
        return Atomics.load(this.#control, STATE);
    }

    finished(): boolean {
        return this.state() !== RING_OPEN && this.available() === 0;
    }

    read(limit: number): Uint8Array {
        const readable = Math.min(limit, this.available());
        if (readable === 0) {
            return new Uint8Array(0);
        }

        const read = Atomics.load(this.#control, READ_CURSOR);
        const start = read & (this.#capacity - 1);
        const untilEnd = Math.min(readable, this.#capacity - start);

        const out = new Uint8Array(readable);
        out.set(this.#data.subarray(start, start + untilEnd), 0);
        if (readable > untilEnd) {
            out.set(this.#data.subarray(0, readable - untilEnd), untilEnd);
        }

        Atomics.store(this.#control, READ_CURSOR, (read + readable) | 0);
        Atomics.notify(this.#control, READ_CURSOR);

        return out;
    }
}
