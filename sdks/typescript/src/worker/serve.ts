import { coreModuleLoaderFor } from "./component-imports.js";
import { Dispatcher } from "./dispatch.js";
import { describeThrown } from "./protocol.js";
import type { BundledCoreModules, ComponentModule } from "./component-imports.js";
import type { Executor } from "./dispatch.js";
import type { WorkerInit, WorkerMessage, WorkerRequest } from "./protocol.js";

const workerScope = globalThis as unknown as {
    postMessage(message: WorkerMessage, transfer?: Transferable[]): void;
    addEventListener(
        type: "message",
        listener: (event: { data: WorkerInit | WorkerRequest }) => void,
    ): void;
};

function loadComponent(
    dispatcher: Dispatcher,
    bundledComponent: ComponentModule<{ executor: Executor }> | undefined,
    message: WorkerInit,
    getCoreModule: ReturnType<typeof coreModuleLoaderFor>,
): Promise<number> {
    if (bundledComponent !== undefined) {
        return dispatcher.loadModule(bundledComponent, getCoreModule);
    }

    if (message.componentUrl === null) {
        return Promise.reject(
            new Error(
                "vpod: this worker loads the component by URL and was not given one. " +
                    "Pass componentUrl, or use the embed worker, which has it bundled in.",
            ),
        );
    }

    return dispatcher.load(message.componentUrl, getCoreModule);
}

export function serveWorker(
    bundledComponent?: ComponentModule<{ executor: Executor }>,
    bundledCoreModules?: BundledCoreModules,
): void {
    const dispatcher = new Dispatcher();

    workerScope.addEventListener("message", (event) => {
        const message = event.data;

        if ("kind" in message && message.kind === "init") {
            const getCoreModule = coreModuleLoaderFor(
                message.coreModules,
                bundledCoreModules,
            );
            const loaded = loadComponent(dispatcher, bundledComponent, message, getCoreModule);

            loaded
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
}
