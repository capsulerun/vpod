/**
 * The Node entry point.
 */

export { createNodeTransport } from "./transport.js";
export { createNodeWorkerTransport } from "./worker-transport.js";
export type { NodeTransportOptions } from "./transport.js";

export { FileSnapshotStore, defaultCacheDirectory } from "./store.js";

export { Commands, Code } from "../sandbox.js";
export type { SandboxOptions, RunOptions, SnapshotSource } from "../sandbox.js";

import { Sandbox as PortableSandbox } from "../sandbox.js";
import type { SandboxOptions } from "../sandbox.js";
import type { SuspendedInstance } from "../instances.js";
import { createNodeWorkerTransport } from "./worker-transport.js";

async function withNodeTransport(options: SandboxOptions): Promise<SandboxOptions> {
    if (options.transport !== undefined) {
        return options;
    }

    return { ...options, transport: await createNodeWorkerTransport() };
}

export type Sandbox = PortableSandbox;

export const Sandbox = {
    create: async (options: SandboxOptions = {}): Promise<PortableSandbox> =>
        PortableSandbox.create(await withNodeTransport(options)),

    resume: async (
        instance: string | SuspendedInstance,
        options: SandboxOptions = {},
    ): Promise<PortableSandbox> =>
        PortableSandbox.resume(instance, await withNodeTransport(options)),

    listInstances: () => PortableSandbox.listInstances(),
    destroy: (instanceId: string) => PortableSandbox.destroy(instanceId),
};

export {
    CommandResult,
    CodeExecution,
    parseCodeOutput,
    normalizeLineEndings,
} from "../execution.js";

export { SandboxRuntime } from "../runtime.js";
export type { SandboxRuntimeOptions, StorageQuota } from "../runtime.js";

export { capabilitiesOf, explainUnreachable } from "../net/capabilities.js";
export type { NetworkBackendName, NetworkCapabilities } from "../net/capabilities.js";

export * as snapshots from "../snapshots/index.js";

export type {
    ExecutionResult,
    PullResult,
    SnapshotMount,
    SuspendResult,
} from "../worker/protocol.js";
