/**
 * Its only job is to own an event loop that keeps turning while the emulator's is blocked inside a guest.
 */

import { FetchDriver } from "./fetch-driver.js";
import type { DriverCommand, DriverOptions } from "./driver-protocol.js";

type ControlMessage =
    | { kind: "configure"; options: DriverOptions }
    | { kind: "attach"; port: MessagePort };

const workerScope = globalThis as unknown as {
    addEventListener(
        type: "message",
        listener: (event: { data: ControlMessage }) => void,
    ): void;
    postMessage(message: unknown): void;
};

let driver = new FetchDriver();

workerScope.addEventListener("message", (event) => {
    const message = event.data;

    if (message.kind === "configure") {
        driver = new FetchDriver(message.options);
        workerScope.postMessage({ kind: "configured" });
        return;
    }

    if (message.kind === "attach") {
        const port = message.port;
        port.addEventListener("message", (portEvent: MessageEvent) => {
            driver.handle(portEvent.data as DriverCommand);
        });
        port.start();
    }
});
