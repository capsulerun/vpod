import { assetUrl, directoryOf, setAssetBaseUrlIfUnset } from "../asset-base.js";
import type { CoreModuleBytes } from "../worker/component-imports.js";
import type { WorkerCall } from "../worker/protocol.js";
import type { ExecutorTransport } from "./types.js";

setAssetBaseUrlIfUnset(directoryOf(import.meta.url, "../"));

export interface InlineTransportOptions {
    componentUrl?: string | URL;
    coreModules?: CoreModuleBytes;
}

class InlineTransport implements ExecutorTransport {
    #dispatcher: { handle(call: WorkerCall): Promise<unknown> };
    #componentLoadMilliseconds: number;

    constructor(
        dispatcher: { handle(call: WorkerCall): Promise<unknown> },
        componentLoadMilliseconds: number,
    ) {
        this.#dispatcher = dispatcher;
        this.#componentLoadMilliseconds = componentLoadMilliseconds;
    }

    ready(): Promise<number> {
        return Promise.resolve(this.#componentLoadMilliseconds);
    }

    call<T>(call: WorkerCall): Promise<T> {
        return this.#dispatcher.handle(call) as Promise<T>;
    }

    terminate(): void {}
}

export async function createInlineTransport(
    options: InlineTransportOptions = {},
): Promise<ExecutorTransport> {
    const { Dispatcher } = await import("../worker/dispatch.js");
    const { coreModuleLoaderFor } = await import("../worker/component-imports.js");
    const dispatcher = new Dispatcher();

    const componentUrl =
        options.componentUrl ?? assetUrl("component/vpod.js");
    const componentLoadMilliseconds = await dispatcher.load(
        String(componentUrl),
        coreModuleLoaderFor(options.coreModules),
    );

    return new InlineTransport(dispatcher, componentLoadMilliseconds);
}
