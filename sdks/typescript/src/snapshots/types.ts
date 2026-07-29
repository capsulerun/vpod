export interface SnapshotEntry {
    id: string;
    name: string;
    tag: string;
    memory_label: string;
    description: string;
    url: string;
    sha256: string;
    size: number;
}

export interface Catalogue {
    version: string;
    snapshots: SnapshotEntry[];
}

export type SnapshotSource = "opfs" | "network";

export interface PulledSnapshot {
    entry: SnapshotEntry;
    bytes: Uint8Array;
    source: SnapshotSource;
    fetchMilliseconds: number;
    verifyMilliseconds: number;
    storeMilliseconds: number;
}
