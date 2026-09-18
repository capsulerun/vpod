export type MountSpec = Record<string, string>;

export interface MountEntry {
    hostAlias: string;
    guestPath: string;
    writable: boolean;
}

const WRITABLE_SUFFIX = ":rw";

export const MOUNTS_NEED_A_HOST =
    "vpod: mounts need a host filesystem, which a browser does not have. " +
    "Create the sandbox without mounts, or run it under Node.";

export function mountEntries(mounts: MountSpec | undefined): MountEntry[] {
    if (mounts === undefined) {
        return [];
    }
    if (typeof mounts !== "object" || mounts === null || Array.isArray(mounts)) {
        throw new Error(
            `vpod: mounts must be an object of host path to guest path, got ${JSON.stringify(mounts)}`,
        );
    }

    return Object.entries(mounts).map(([hostPath, guestSpec]) => {
        if (typeof guestSpec !== "string" || guestSpec.length === 0) {
            throw new Error(`vpod: mount ${JSON.stringify(hostPath)} needs a guest path`);
        }

        const writable = guestSpec.endsWith(WRITABLE_SUFFIX);
        const guestPath = writable ? guestSpec.slice(0, -WRITABLE_SUFFIX.length) : guestSpec;

        if (!guestPath.startsWith("/")) {
            throw new Error(
                `vpod: mount ${JSON.stringify(hostPath)} must name an absolute guest path, got ${JSON.stringify(guestPath)}`,
            );
        }

        return { hostAlias: hostPath, guestPath, writable };
    });
}
