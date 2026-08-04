/**
 * Types for the parts of `@bytecodealliance/preview2-shim` this SDK builds on.
 */

interface VendorPollable {
    ready(): boolean;
    block(): Promise<void>;
}

declare module "@bytecodealliance/preview2-shim/io" {
    export const error: unknown;
    export const streams: unknown;
    export const poll: {
        Pollable: new (settled?: Promise<unknown>) => VendorPollable;
        poll(list: object[]): Uint32Array | Promise<Uint32Array>;
    };
}

declare module "@bytecodealliance/preview2-shim/clocks" {
    export const monotonicClock: {
        resolution(): bigint;
        now(): bigint;
        subscribeDuration(duration: bigint): VendorPollable;
        subscribeInstant(instant: bigint): VendorPollable;
    };
    export const wallClock: {
        now(): { seconds: bigint; nanoseconds: number };
        resolution(): { seconds: bigint; nanoseconds: number };
    };
}

declare module "@bytecodealliance/preview2-shim/filesystem" {
    /** A directory holds `dir`, a file holds `source`. */
    export interface FileDataEntry {
        dir?: Record<string, FileDataEntry>;
        source?: Uint8Array | string;
    }

    export function _setFileData(fileData: FileDataEntry): void;
    export function _getFileData(): string;
    export function _setPreopens(preopens: Record<string, FileDataEntry>): void;
    export function _addPreopen(virtualPath: string, fileData: FileDataEntry): void;
    export function _clearPreopens(): void;
    export function _getPreopens(): Array<[unknown, string]>;
    export function _createPreopenDescriptor(fileData: FileDataEntry): unknown;
    export function _setCwd(cwd: string): void;

    export const preopens: unknown;
    export const types: unknown;
}

declare module "@bytecodealliance/preview2-shim/cli" {
    export const environment: unknown;
    export const exit: unknown;
    export const stderr: unknown;
    export const stdin: unknown;
    export const stdout: unknown;
    export const terminalInput: unknown;
    export const terminalOutput: unknown;
    export const terminalStderr: unknown;
    export const terminalStdin: unknown;
    export const terminalStdout: unknown;
}

declare module "@bytecodealliance/preview2-shim/random" {
    export const random: unknown;
    export const insecure: unknown;
    export const insecureSeed: unknown;
}
