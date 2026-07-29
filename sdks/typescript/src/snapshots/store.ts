const STORE_DIRECTORY = "vpod-snapshots";

interface SyncAccessHandle {
    read(buffer: Uint8Array, options?: { at?: number }): number;
    write(buffer: Uint8Array, options?: { at?: number }): number;
    truncate(size: number): void;
    getSize(): number;
    flush(): void;
    close(): void;
}

interface FileHandle {
    createSyncAccessHandle(): Promise<SyncAccessHandle>;
}

interface DirectoryHandle {
    getFileHandle(name: string, options?: { create?: boolean }): Promise<FileHandle>;
    getDirectoryHandle(
        name: string,
        options?: { create?: boolean },
    ): Promise<DirectoryHandle>;
    removeEntry(name: string, options?: { recursive?: boolean }): Promise<void>;
}

export class SnapshotStore {
    #directory: DirectoryHandle;

    private constructor(directory: DirectoryHandle) {
        this.#directory = directory;
    }

    static available(): boolean {
        return typeof navigator !== "undefined" && navigator.storage?.getDirectory !== undefined;
    }

    static async open(): Promise<SnapshotStore> {
        const root = (await navigator.storage.getDirectory()) as unknown as DirectoryHandle;
        const directory = await root.getDirectoryHandle(STORE_DIRECTORY, { create: true });
        return new SnapshotStore(directory);
    }

    async read(name: string): Promise<Uint8Array | null> {
        let handle: SyncAccessHandle;
        try {
            const file = await this.#directory.getFileHandle(name);
            handle = await file.createSyncAccessHandle();
        } catch {
            return null;
        }

        try {
            const bytes = new Uint8Array(handle.getSize());
            handle.read(bytes, { at: 0 });
            return bytes;
        } finally {
            handle.close();
        }
    }

    async write(name: string, bytes: Uint8Array): Promise<void> {
        const file = await this.#directory.getFileHandle(name, { create: true });
        const handle = await file.createSyncAccessHandle();
        try {
            handle.truncate(0);
            handle.write(bytes, { at: 0 });
            handle.flush();
        } finally {
            handle.close();
        }
    }

    async readText(name: string): Promise<string | null> {
        const bytes = await this.read(name);
        return bytes === null ? null : new TextDecoder().decode(bytes);
    }

    async writeText(name: string, text: string): Promise<void> {
        await this.write(name, new TextEncoder().encode(text));
    }

    async remove(name: string): Promise<void> {
        try {
            await this.#directory.removeEntry(name);
        } catch {
            // Already gone
        }
    }

    /**
     * Browser storage quota.
     */
    static async quota(): Promise<{ usage: number; quota: number } | null> {
        if (navigator.storage?.estimate === undefined) {
            return null;
        }
        const estimate = await navigator.storage.estimate();
        return { usage: estimate.usage ?? 0, quota: estimate.quota ?? 0 };
    }
}
