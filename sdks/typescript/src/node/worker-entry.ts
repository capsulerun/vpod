/**
 * Runs the guest on a `worker_threads` worker, so a multi-second `session-exec` does not stall the calling thread's event loop.
 */

import { parentPort, workerData } from "node:worker_threads";

import { loadNodeDispatcher } from "./transport.js";
import type { NodeTransportOptions } from "./transport.js";
import type { WorkerCall } from "../worker/protocol.js";

interface Request {
    id: number;
    call: WorkerCall;
}

const port = parentPort;
if (port === null) {
    throw new Error("vpod: worker-entry.js must be run as a worker_threads worker");
}

function describe(thrown: unknown): string {
    if (thrown instanceof Error) {
        return thrown.stack ?? thrown.message;
    }
    return String((thrown as { payload?: unknown })?.payload ?? thrown);
}

const dispatcher = await loadNodeDispatcher((workerData ?? {}) as NodeTransportOptions);
port.postMessage({ kind: "ready", componentLoadMilliseconds: dispatcher.loadMilliseconds });

port.on("message", (request: Request) => {
    void (async () => {
        try {
            const value = await dispatcher.handle(request.call);
            port.postMessage({ id: request.id, ok: true, value });
        } catch (thrown: unknown) {
            port.postMessage({ id: request.id, ok: false, error: describe(thrown) });
        }
    })();
});
