export interface WorkerInit {
    kind: "init";
    componentUrl: string;
    channels?: Record<string, SharedArrayBuffer>;
}

export interface ExecutionResult {
    stdout: string;
    stderr: string;
    exitCode: number;
}

export interface PullResult {
    snapshotPath: string;
    id: string;
    byteLength: number;
    source: "opfs" | "disk" | "network";
    fetchMilliseconds: number;
    verifyMilliseconds: number;
    storeMilliseconds: number;
}

export interface SnapshotMount {
    snapshotPath: string;
    byteLength: number;
}

export interface SuspendResult {
    deltaBytes: ArrayBuffer;
    byteLength: number;
}

export type WorkerCall =
    | { kind: "pull-snapshot"; name?: string; registryUrl?: string; force?: boolean }
    | { kind: "storage-quota" }
    | { kind: "fetch-snapshot"; url: string; name?: string }
    | { kind: "mount-snapshot"; name: string; bytes: ArrayBuffer }
    | { kind: "session-start"; snapshotPath: string; command: string; prompt: string }
    | {
          kind: "session-exec";
          handle: bigint;
          code: string;
          timeoutSeconds: bigint | null;
      }
    | { kind: "session-close"; handle: bigint }
    | { kind: "session-suspend"; handle: bigint }
    | {
          kind: "session-resume";
          snapshotPath: string;
          deltaBytes: ArrayBuffer;
          command: string;
          prompt: string;
      }
    | { kind: "poll-stats" }
    | { kind: "component-load-milliseconds" }
    | { kind: "enable-network"; port: MessagePort; allowedPorts?: number[] };

export interface WorkerRequest {
    id: number;
    call: WorkerCall;
}

export type WorkerReply =
    | { id: number; ok: true; value: unknown }
    | { id: number; ok: false; error: string };

export interface WorkerReady {
    kind: "ready";
    componentLoadMilliseconds: number;
}

export type WorkerMessage = WorkerReply | WorkerReady;

export function describeThrown(thrown: unknown): string {
    if (thrown instanceof Error) {
        return thrown.stack ?? thrown.message;
    }
    return String((thrown as { payload?: unknown })?.payload ?? thrown);
}
