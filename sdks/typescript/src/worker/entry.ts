import { Dispatcher } from "./dispatch.js";
import { describeThrown } from "./protocol.js";
import type { WorkerInit, WorkerMessage, WorkerRequest } from "./protocol.js";

const workerScope = globalThis as unknown as {
    postMessage(message: WorkerMessage, transfer?: Transferable[]): void;
    addEventListener(
        type: "message",
        listener: (event: { data: WorkerInit | WorkerRequest }) => void,
    ): void;
};

const dispatcher = new Dispatcher();

workerScope.addEventListener("message", (event) => {
    const message = event.data;

    if ("kind" in message && message.kind === "init") {
        dispatcher
            .load(message.componentUrl)
            .then((componentLoadMilliseconds) => {
                workerScope.postMessage({ kind: "ready", componentLoadMilliseconds });
            })
            .catch((thrown: unknown) => {
                workerScope.postMessage({ id: -1, ok: false, error: describeThrown(thrown) });
            });
        return;
    }

    const request = message as WorkerRequest;
    void (async () => {
        try {
            const value = await dispatcher.handle(request.call);
            workerScope.postMessage({ id: request.id, ok: true, value });
        } catch (thrown: unknown) {
            workerScope.postMessage({
                id: request.id,
                ok: false,
                error: describeThrown(thrown),
            });
        }
    })();
});
