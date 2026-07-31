/**
 * Snapshot cache on the host filesystem.
 */

import { homedir, platform } from "node:os";
import { join } from "node:path";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";

import type { SnapshotStorage } from "../snapshots/store.js";

export function defaultCacheDirectory(): string {
    const override = process.env.VPOD_CACHE_DIR;
    if (override !== undefined && override !== "") {
        return override;
    }

    if (platform() === "darwin") {
        return join(homedir(), "Library", "Application Support", "vpod", "snapshots");
    }

    if (platform() === "win32") {
        const appData = process.env.LOCALAPPDATA ?? join(homedir(), "AppData", "Local");
        return join(appData, "vpod", "snapshots");
    }

    const dataHome = process.env.XDG_DATA_HOME ?? join(homedir(), ".local", "share");
    return join(dataHome, "vpod", "snapshots");
}

export class FileSnapshotStore implements SnapshotStorage {
    readonly kind = "disk" as const;
    readonly directory: string;

    constructor(directory: string = defaultCacheDirectory()) {
        this.directory = directory;
    }

    async ready(): Promise<this> {
        await mkdir(this.directory, { recursive: true });
        return this;
    }

    pathFor(name: string): string {
        return join(this.directory, name);
    }

    async read(name: string): Promise<Uint8Array | null> {
        try {
            return new Uint8Array(await readFile(this.pathFor(name)));
        } catch {
            return null;
        }
    }

    async write(name: string, bytes: Uint8Array): Promise<void> {
        await mkdir(this.directory, { recursive: true });
        await writeFile(this.pathFor(name), bytes);
    }

    async readText(name: string): Promise<string | null> {
        try {
            return await readFile(this.pathFor(name), "utf8");
        } catch {
            return null;
        }
    }

    async writeText(name: string, text: string): Promise<void> {
        await mkdir(this.directory, { recursive: true });
        await writeFile(this.pathFor(name), text, "utf8");
    }

    async remove(name: string): Promise<void> {
        await rm(this.pathFor(name), { force: true });
    }
}
