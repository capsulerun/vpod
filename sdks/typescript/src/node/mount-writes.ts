/**
 * Write access to a mounted host directory, for the Node target.
 */

import * as filesystem from "@bytecodealliance/preview2-shim/filesystem";

interface DescriptorFlags {
    mutateDirectory?: boolean;
}

type OpenAt = (
    this: unknown,
    pathFlags: unknown,
    path: string,
    openFlags: unknown,
    descriptorFlags: DescriptorFlags,
    ...rest: unknown[]
) => unknown;

const UNSUPPORTED = "unsupported";

let applied = false;

export function allowMountWrites(): void {
    const descriptor = (filesystem.types as { Descriptor?: { prototype: Record<string, unknown> } })
        .Descriptor;
    const original = descriptor?.prototype.openAt as OpenAt | undefined;

    if (applied || descriptor === undefined || original === undefined) {
        return;
    }
    applied = true;

    descriptor.prototype.openAt = function (
        this: unknown,
        pathFlags: unknown,
        path: string,
        openFlags: unknown,
        descriptorFlags: DescriptorFlags,
        ...rest: unknown[]
    ) {
        try {
            return original.call(this, pathFlags, path, openFlags, descriptorFlags, ...rest);
        } catch (thrown: unknown) {
            if (thrown !== UNSUPPORTED || descriptorFlags?.mutateDirectory !== true) {
                throw thrown;
            }
            return original.call(
                this,
                pathFlags,
                path,
                openFlags,
                { ...descriptorFlags, mutateDirectory: false },
                ...rest,
            );
        }
    } as unknown as typeof descriptor.prototype.openAt;
}
