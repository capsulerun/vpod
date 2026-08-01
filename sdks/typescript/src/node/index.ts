/**
 * The Node entry point.
 */

export { createNodeTransport } from "./transport.js";
export { createNodeWorkerTransport } from "./worker-transport.js";
export type { NodeTransportOptions } from "./transport.js";

export { FileSnapshotStore, defaultCacheDirectory } from "./store.js";

export { Sandbox, Commands, Code } from "../sandbox.js";
export type { SandboxOptions, RunOptions, SnapshotSource } from "../sandbox.js";

import { setDefaultTransportFactory } from "../transport/default.js";
import { createNodeWorkerTransport } from "./worker-transport.js";

setDefaultTransportFactory(() => createNodeWorkerTransport());

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
