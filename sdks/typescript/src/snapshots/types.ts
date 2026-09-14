/** A snapshot's own engine: a whole engine component built from a trace of that snapshot. */
export interface EngineEntry {
    vpod_version: string;
    interface: string;
    url: string;
    sha256: string;
    size: number;
}

export interface SnapshotEntry {
    id: string;
    name: string;
    tag: string;
    memory_label: string;
    description: string;
    url: string;
    sha256: string;
    size: number;
    engines?: EngineEntry[];
}

export interface Catalogue {
    version: string;
    snapshots: SnapshotEntry[];
}

export type SnapshotSource = "opfs" | "disk" | "network";

export interface PulledSnapshot {
    entry: SnapshotEntry;
    bytes: Uint8Array;
    source: SnapshotSource;
    fetchMilliseconds: number;
    verifyMilliseconds: number;
    storeMilliseconds: number;
}
