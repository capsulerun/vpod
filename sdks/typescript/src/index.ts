import { directoryOf, setAssetBaseUrl } from "./asset-base.js";

setAssetBaseUrl(directoryOf(import.meta.url, "./"));

export { Sandbox, Commands, Code, Execution } from "./sandbox.js";
export type { SandboxOptions, RunOptions, SnapshotSource, Stdin } from "./sandbox.js";

export {
    CommandResult,
    CodeExecution,
    parseCodeOutput,
    normalizeLineEndings,
} from "./execution.js";

export { SandboxRuntime } from "./runtime.js";
export type { SandboxRuntimeOptions, StorageQuota } from "./runtime.js";

export { InstanceStore } from "./instances.js";
export type { InstanceRecord, SuspendedInstance } from "./instances.js";

export * as snapshots from "./snapshots/index.js";
export { SnapshotAuthError } from "./snapshots/index.js";

export { WorkerTransport } from "./transport/worker.js";
export type { WorkerTransportOptions } from "./transport/worker.js";
export { createInlineTransport } from "./transport/inline.js";
export type { ExecutorTransport } from "./transport/types.js";

export { networkAvailability } from "./net/availability.js";
export type { NetworkAvailability } from "./net/availability.js";

export { capabilitiesOf, explainUnreachable } from "./net/capabilities.js";
export type { NetworkBackendName, NetworkCapabilities } from "./net/capabilities.js";

export type {
    ExecutionResult,
    PullResult,
    SnapshotMount,
    SuspendResult,
} from "./worker/protocol.js";

export { setSocketBackend, socketBackendName } from "./shims/sockets.js";
export type {
    SocketBackend,
    TcpConnection,
    UdpConnection,
    AddressResolution,
} from "./shims/sockets.js";
