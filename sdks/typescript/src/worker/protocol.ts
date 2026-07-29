/**
 * Messages between the page and the Worker the emulator runs in.
 */

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

export type WorkerCall =
    | { kind: "fetch-snapshot"; url: string; name?: string }
    | { kind: "mount-snapshot"; name: string; bytes: ArrayBuffer }
    | {
          kind: "session-start";
          snapshotPath: string;
          command: string;
          prompt: string;
      }
    | {
          kind: "session-exec";
          handle: bigint;
          code: string;
          timeoutSeconds: bigint | null;
      }
    | { kind: "session-close"; handle: bigint }
    | { kind: "session-suspend"; handle: bigint; deltaPath: string }
    | {
          kind: "session-resume";
          snapshotPath: string;
          deltaPath: string;
          command: string;
          prompt: string;
      }
    | { kind: "poll-stats" }
    | { kind: "component-load-milliseconds" };

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
