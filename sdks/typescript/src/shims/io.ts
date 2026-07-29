import { error, poll as vendorPoll, streams } from "@bytecodealliance/preview2-shim/io";
import { synchronousReadinessOf } from "./pollable.js";

/**
 * Replaces the stock browser `wasi:io/poll`.
 */

let spinCount = 0;
let spinNanoseconds = 0;

export function pollStats(): { spinCount: number; spinNanoseconds: number } {
    return { spinCount, spinNanoseconds };
}

export function resetPollStats(): void {
    spinCount = 0;
    spinNanoseconds = 0;
}

interface VendorPollable {
    ready(): boolean;
}

function isReady(pollable: object): boolean {
    const readiness = synchronousReadinessOf(pollable);

    if (readiness !== undefined) {
        return readiness.isReady();
    }

    return (pollable as VendorPollable).ready();
}

function readyIndices(list: object[]): number[] {
    const ready: number[] = [];

    for (let index = 0; index < list.length; index++) {
        if (isReady(list[index]!)) {
            ready.push(index);
        }
    }

    return ready;
}

function anyReady(list: object[]): boolean {
    for (const pollable of list) {
        if (isReady(pollable)) {
            return true;
        }
    }

    return false;
}

function pollList(list: object[]): Uint32Array {
    if (list.length === 0) {
        throw new Error("poll list must not be empty");
    }

    const readyNow = readyIndices(list);
    if (readyNow.length > 0) {
        return new Uint32Array(readyNow);
    }

    let earliestDeadline = Infinity;
    for (const pollable of list) {
        const readiness = synchronousReadinessOf(pollable);
        if (
            readiness !== undefined &&
            readiness.readyAtMilliseconds !== null &&
            readiness.readyAtMilliseconds < earliestDeadline
        ) {
            earliestDeadline = readiness.readyAtMilliseconds;
        }
    }

    if (earliestDeadline === Infinity) {
        throw new Error(
            "vpod: wasi:io/poll was asked to block on a pollable with no " +
                "synchronous readiness source. The library world only ever polls a " +
                "monotonic-clock timer, so waiting here would need the event loop " +
                "that synchronous wasm cannot yield to. A new blocking site reached " +
                "this shim.",
        );
    }

    const startedAt = performance.now();
    while (performance.now() < earliestDeadline) {
        if (anyReady(list)) {
            break;
        }
    }

    spinCount += 1;
    spinNanoseconds += (performance.now() - startedAt) * 1_000_000;

    const readyAfterWait = readyIndices(list);
    if (readyAfterWait.length > 0) {
        return new Uint32Array(readyAfterWait);
    }

    return new Uint32Array(list.map((_, index) => index));
}

export const poll = {
    Pollable: vendorPoll.Pollable,
    poll: pollList,
    pollList,
    pollOne: (pollable: object): Uint32Array => pollList([pollable]),
};

export { error, streams };
