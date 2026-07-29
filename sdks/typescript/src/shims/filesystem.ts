import { _setFileData } from "@bytecodealliance/preview2-shim/filesystem";
export * from "@bytecodealliance/preview2-shim/filesystem";

/** Virtual directory the guest sees snapshots under. */
export const SNAPSHOT_DIRECTORY = "snap";

export function mountSnapshot(name: string, bytes: Uint8Array): string {
    _setFileData({
        dir: { [SNAPSHOT_DIRECTORY]: { dir: { [name]: { source: bytes } } } },
    });
    return `${SNAPSHOT_DIRECTORY}/${name}`;
}
