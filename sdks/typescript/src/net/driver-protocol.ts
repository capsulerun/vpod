/**
 * Messages between the emulator worker and the thread that runs `fetch`.
 */

export type DriverCommand =
    | {
          kind: "open";
          id: number;
          ring: SharedArrayBuffer;
          resolvedHostname: string | undefined;
          port: number;
      }
    | { kind: "send"; id: number; bytes: ArrayBuffer }
    | { kind: "shutdown"; id: number }
    | { kind: "close"; id: number };

export interface DriverOptions {
    requestTimeoutMilliseconds?: number;
}
