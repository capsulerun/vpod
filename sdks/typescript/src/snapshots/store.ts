const STORE_DIRECTORY = "vpod-snapshots";

interface SyncAccessHandle {
    read(buffer: Uint8Array, options?: { at?: number }): number;
    write(buffer: Uint8Array, options?: { at?: number }): number;
    truncate(size: number): void;
    getSize(): number;
    flush(): void;
    close(): void;
}

interface WritableStream {
    write(data: BufferSource): Promise<void>;
    close(): Promise<void>;
}

interface FileHandle {
    createSyncAccessHandle?(): Promise<SyncAccessHandle>;
    createWritable?(options?: { keepExistingData?: boolean }): Promise<WritableStream>;
    getFile?(): Promise<Blob>;
}

interface DirectoryHandle {
    getFileHandle(name: string, options?: { create?: boolean }): Promise<FileHandle>;
    getDirectoryHandle(
        name: string,
        options?: { create?: boolean },
    ): Promise<DirectoryHandle>;
    removeEntry(name: string, options?: { recursive?: boolean }): Promise<void>;
    entries?(): AsyncIterableIterator<[string, FileHandle]>;
}

export interface CachedFile {
    name: string;
    byteLength: number;
}

export interface SnapshotStorage {
    readonly kind: "opfs" | "disk";
    read(name: string): Promise<Uint8Array | null>;
    write(name: string, bytes: Uint8Array): Promise<void>;
    readText(name: string): Promise<string | null>;
    writeText(name: string, text: string): Promise<void>;
    remove(name: string): Promise<void>;
    list(): Promise<CachedFile[]>;
}

async function sizeOf(handle: FileHandle): Promise<number> {
    if (handle.getFile !== undefined) {
        return (await handle.getFile()).size;
    }

    if (handle.createSyncAccessHandle !== undefined) {
        const open = await handle.createSyncAccessHandle();
        try {
            return open.getSize();
        } finally {
            open.close();
        }
    }

    return 0;
}

export class SnapshotStore implements SnapshotStorage {
    readonly kind = "opfs" as const;

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
        let file: FileHandle;
        try {
            file = await this.#directory.getFileHandle(name);
        } catch {
            return null;
        }

        const openSync = file.createSyncAccessHandle;
        if (openSync !== undefined) {

            let handle: SyncAccessHandle;
            try {
                handle = await openSync.call(file);
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

        if (file.getFile === undefined) {
            throw new Error("vpod: this file handle can neither be read synchronously nor as a File");
        }
        return new Uint8Array(await (await file.getFile()).arrayBuffer());
    }

    async write(name: string, bytes: Uint8Array): Promise<void> {
        const file = await this.#directory.getFileHandle(name, { create: true });

        const openSync = file.createSyncAccessHandle;
        if (openSync !== undefined) {
            const handle = await openSync.call(file);
            try {
                handle.truncate(0);
                handle.write(bytes, { at: 0 });
                handle.flush();
            } finally {
                handle.close();
            }
            return;
        }

        const openWritable = file.createWritable;
        if (openWritable === undefined) {
            throw new Error(
                "vpod: this file handle supports neither createSyncAccessHandle nor " +
                    "createWritable, so nothing can be stored here",
            );
        }

        const writable = await openWritable.call(file);
        await writable.write(bytes as BufferSource);
        await writable.close();
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

    async list(): Promise<CachedFile[]> {
        const entries = this.#directory.entries;
        if (entries === undefined) {
            return [];
        }

        const files: CachedFile[] = [];
        for await (const [name, handle] of entries.call(this.#directory)) {
            files.push({ name, byteLength: await sizeOf(handle) });
        }
        return files;
    }

    static async persist(): Promise<boolean> {
        if (navigator.storage?.persist === undefined) {
            return false;
        }
        return navigator.storage.persist();
    }

    static async persisted(): Promise<boolean> {
        if (navigator.storage?.persisted === undefined) {
            return false;
        }
        return navigator.storage.persisted();
    }

    static async quota(): Promise<{ usage: number; quota: number } | null> {
        if (navigator.storage?.estimate === undefined) {
            return null;
        }
        const estimate = await navigator.storage.estimate();
        return { usage: estimate.usage ?? 0, quota: estimate.quota ?? 0 };
    }
}
