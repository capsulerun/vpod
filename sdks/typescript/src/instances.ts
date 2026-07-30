import { SnapshotStore } from "./snapshots/store.js";

const MANIFEST_FILE = "instances.json";

export interface InstanceRecord {
    id: string;
    snapshotId: string;
    savedAt: number;
    byteLength: number;
}

export interface SuspendedInstance {
    id: string;
    snapshotId: string;
    delta: Uint8Array;
}

export class InstanceStore {
    #store: SnapshotStore;

    private constructor(store: SnapshotStore) {
        this.#store = store;
    }

    static available(): boolean {
        return SnapshotStore.available();
    }

    static async open(): Promise<InstanceStore> {
        if (!SnapshotStore.available()) {
            throw new Error(
                "vpod: origin-private storage is unavailable here, so suspended " +
                    "instances cannot be stored. Use sandbox.suspend() and keep the " +
                    "delta bytes yourself.",
            );
        }
        return new InstanceStore(await SnapshotStore.open());
    }

    async save(snapshotId: string, delta: Uint8Array): Promise<string> {
        const id = crypto.randomUUID();
        await this.#store.write(`${id}.delta`, delta);

        const manifest = await this.list();
        manifest.push({ id, snapshotId, savedAt: Date.now(), byteLength: delta.byteLength });
        await this.#writeManifest(manifest);

        return id;
    }

    async load(id: string): Promise<SuspendedInstance> {
        const record = (await this.list()).find((entry) => entry.id === id);
        if (record === undefined) {
            throw new Error(`vpod: no suspended instance '${id}'`);
        }

        const delta = await this.#store.read(`${id}.delta`);
        if (delta === null) {
            await this.remove(id);
            throw new Error(
                `vpod: instance '${id}' is in the manifest but its delta is gone. ` +
                    `Origin-private storage is evictable, so a suspended instance can ` +
                    `outlive its bytes.`,
            );
        }

        return { id, snapshotId: record.snapshotId, delta };
    }

    async list(): Promise<InstanceRecord[]> {
        const text = await this.#store.readText(MANIFEST_FILE);
        if (text === null) {
            return [];
        }
        try {
            return JSON.parse(text) as InstanceRecord[];
        } catch {
            return [];
        }
    }

    async remove(id: string): Promise<void> {
        await this.#store.remove(`${id}.delta`);
        await this.#writeManifest((await this.list()).filter((entry) => entry.id !== id));
    }

    async #writeManifest(records: InstanceRecord[]): Promise<void> {
        await this.#store.writeText(MANIFEST_FILE, JSON.stringify(records));
    }
}
