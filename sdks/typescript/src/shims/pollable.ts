import { poll } from "@bytecodealliance/preview2-shim/io";

/**
 * Readiness that a `poll` call can evaluate without yielding to the event loop.
 */
export interface SynchronousReadiness {
    isReady(): boolean;

    // The `performance.now()` millisecond at which `isReady` starts returning true, or null when readiness depends on something other than time passing.
    readyAtMilliseconds: number | null;
}

const readinessSources = new WeakMap<object, SynchronousReadiness>();

// Attach a synchronous readiness source to a pollable.
export function registerSynchronousReadiness(
    pollable: object,
    readiness: SynchronousReadiness,
): void {
    readinessSources.set(pollable, readiness);
}

export function synchronousReadinessOf(
    pollable: object,
): SynchronousReadiness | undefined {
    return readinessSources.get(pollable);
}

const PollableClass = poll.Pollable as unknown as {
    new (settled?: Promise<unknown>): object;
};

// A pollable that becomes ready purely by the clock running forward.
export class TimerPollable extends PollableClass {
    readonly readyAtMilliseconds: number;

    constructor(readyAtMilliseconds: number) {

        super(new Promise<never>(() => {}));

        this.readyAtMilliseconds = readyAtMilliseconds;
        registerSynchronousReadiness(this, {
            isReady: () => performance.now() >= readyAtMilliseconds,
            readyAtMilliseconds,
        });
    }

    ready(): boolean {
        return performance.now() >= this.readyAtMilliseconds;
    }
}
