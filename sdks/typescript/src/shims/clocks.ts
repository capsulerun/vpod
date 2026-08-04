import { wallClock } from "@bytecodealliance/preview2-shim/clocks";
import { TimerPollable } from "./pollable.js";

const NANOSECONDS_PER_MILLISECOND = 1_000_000;

/**
 * Replaces the stock browser clocks shim.
 */
export const monotonicClock = {
    resolution(): bigint {
        return BigInt(NANOSECONDS_PER_MILLISECOND);
    },

    now(): bigint {
        return BigInt(Math.floor(performance.now() * NANOSECONDS_PER_MILLISECOND));
    },

    subscribeDuration(durationNanoseconds: bigint): object {
        const milliseconds =
            Number(BigInt(durationNanoseconds)) / NANOSECONDS_PER_MILLISECOND;
        return new TimerPollable(performance.now() + Math.max(milliseconds, 0));
    },

    subscribeInstant(instantNanoseconds: bigint): object {
        const target = BigInt(instantNanoseconds);
        const now = monotonicClock.now();
        return monotonicClock.subscribeDuration(target > now ? target - now : 0n);
    },
};

export { wallClock };
