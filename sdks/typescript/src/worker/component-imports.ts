import * as cli from "../shims/cli.js";
import * as clocks from "../shims/clocks.js";
import * as filesystem from "../shims/filesystem.js";
import * as io from "../shims/io.js";
import * as random from "../shims/random.js";
import * as sockets from "../shims/sockets.js";

export const componentImports: Record<string, unknown> = {
    "../shims/cli.js": cli,
    "../shims/clocks.js": clocks,
    "../shims/filesystem.js": filesystem,
    "../shims/io.js": io,
    "../shims/random.js": random,
    "../shims/sockets.js": sockets,
};

export type CoreModuleLoader = (name: string) => Promise<WebAssembly.Module>;

export type CoreModuleBytes = BufferSource | Record<string, BufferSource>;

export interface BundledCoreModules {
    inlined: Record<string, string>;
    required: string[];
}

const isBufferSource = (value: CoreModuleBytes): value is BufferSource =>
    value instanceof ArrayBuffer || ArrayBuffer.isView(value);

function decodeBase64(source: string): ArrayBuffer {
    const binary = atob(source);
    const buffer = new ArrayBuffer(binary.length);
    const bytes = new Uint8Array(buffer);
    for (let index = 0; index < binary.length; index++) {
        bytes[index] = binary.charCodeAt(index);
    }
    return buffer;
}

function suppliedByName(
    bytes: CoreModuleBytes,
    bundled: BundledCoreModules | undefined,
): Record<string, BufferSource> {
    if (!isBufferSource(bytes)) {
        return bytes;
    }

    const required = bundled?.required ?? [];
    if (required.length !== 1) {
        throw new Error(
            `vpod: coreModules was given a single buffer, but this build needs ` +
                (required.length === 0
                    ? `a map keyed by module name.`
                    : `${required.length}: ${required.join(", ")}.`),
        );
    }
    return { [required[0]]: bytes };
}

export function coreModuleLoaderFor(
    bytes: CoreModuleBytes | undefined,
    bundled?: BundledCoreModules,
): CoreModuleLoader | undefined {
    if (bytes === undefined) {
        return undefined;
    }

    const supplied = suppliedByName(bytes, bundled);

    return async (name: string) => {
        const source = supplied[name];
        if (source !== undefined) {
            return WebAssembly.compile(source);
        }

        const inlined = bundled?.inlined[name];
        if (inlined !== undefined) {
            return WebAssembly.compile(decodeBase64(inlined));
        }

        const names = Object.keys(supplied).join(", ") || "nothing";
        throw new Error(
            `vpod: no bytes were supplied for core module '${name}' (got ${names}).`,
        );
    };
}

export interface ComponentModule<T> {
    instantiate(
        getCoreModule: CoreModuleLoader | undefined,
        imports: Record<string, unknown>,
    ): Promise<T>;
}
