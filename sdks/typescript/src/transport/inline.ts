import { assetUrl, directoryOf, setAssetBaseUrlIfUnset } from "../asset-base.js";
import type { WorkerCall } from "../worker/protocol.js";
import type { ExecutorTransport } from "./types.js";

// Set here as well as in index.ts so `vpod/inline` works when imported directly.
setAssetBaseUrlIfUnset(directoryOf(import.meta.url, "../"));

export interface InlineTransportOptions {
    componentUrl?: string | URL;
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
    const dispatcher = new Dispatcher();

    const componentUrl =
        options.componentUrl ?? assetUrl("component/vpod.js");
    const componentLoadMilliseconds = await dispatcher.load(String(componentUrl));

    return new InlineTransport(dispatcher, componentLoadMilliseconds);
}
