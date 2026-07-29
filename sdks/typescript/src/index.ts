export { SandboxRuntime } from "./runtime.js";
export type {
    SandboxRuntimeOptions,
    SnapshotMount,
} from "./runtime.js";
export type { ExecutionResult, PullResult } from "./worker/protocol.js";
export { DEFAULT_REGISTRY_URL, resolveSnapshot } from "./snapshots/catalogue.js";
export { evictById } from "./snapshots/pull.js";
export type { Catalogue, SnapshotEntry } from "./snapshots/types.js";
export { setSocketBackend, socketBackendName } from "./shims/sockets.js";
export type {
    SocketBackend,
    TcpConnection,
    UdpConnection,
    AddressResolution,
} from "./shims/sockets.js";
