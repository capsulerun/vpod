import { _setFileData, type FileDataEntry } from "@bytecodealliance/preview2-shim/filesystem";

export * from "@bytecodealliance/preview2-shim/filesystem";

export const SNAPSHOT_DIRECTORY = "snap";
export const DELTA_DIRECTORY = "deltas";

const root: FileDataEntry = {
    dir: {
        [SNAPSHOT_DIRECTORY]: { dir: {} },
        [DELTA_DIRECTORY]: { dir: {} },
    },
};

_setFileData(root);

function directoryAt(segments: string[]): Record<string, FileDataEntry> {
    let current = root;
    for (const segment of segments) {
        if (current.dir === undefined) {
            throw new Error(`vpod: ${segment} is not a directory in the guest tree`);
        }
        current.dir[segment] ??= { dir: {} };
        current = current.dir[segment];
    }
    if (current.dir === undefined) {
        throw new Error("vpod: expected a directory in the guest tree");
    }
    return current.dir;
}

function writeFile(path: string, bytes: Uint8Array): string {
    const segments = path.split("/");
    const fileName = segments.pop()!;
    directoryAt(segments)[fileName] = { source: bytes };
    return path;
}

export function mountSnapshot(name: string, bytes: Uint8Array): string {
    return writeFile(`${SNAPSHOT_DIRECTORY}/${name}`, bytes);
}

export function mountDelta(name: string, bytes: Uint8Array): string {
    return writeFile(`${DELTA_DIRECTORY}/${name}`, bytes);
}

export function deltaPath(name: string): string {
    return `${DELTA_DIRECTORY}/${name}`;
}

export function readGuestFile(path: string): Uint8Array | null {
    const segments = path.split("/");
    let current: FileDataEntry | undefined = root;

    for (const segment of segments) {
        current = current?.dir?.[segment];
        if (current === undefined) {
            return null;
        }
    }

    const source = current.source;
    if (source === undefined) {
        return null;
    }
    return typeof source === "string" ? new TextEncoder().encode(source) : source;
}

export function removeGuestFile(path: string): void {
    const segments = path.split("/");
    const fileName = segments.pop()!;
    let current: FileDataEntry | undefined = root;

    for (const segment of segments) {
        current = current?.dir?.[segment];
        if (current === undefined) {
            return;
        }
    }

    if (current.dir !== undefined) {
        delete current.dir[fileName];
    }
}
