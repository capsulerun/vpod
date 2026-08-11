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

export type CoreModuleBytes = Record<string, BufferSource>;

export function coreModuleLoaderFor(
    bytes: CoreModuleBytes | undefined,
): CoreModuleLoader | undefined {
    if (bytes === undefined) {
        return undefined;
    }

    return async (name: string) => {
        const source = bytes[name];
        if (source === undefined) {
            const supplied = Object.keys(bytes).join(", ") || "nothing";
            throw new Error(
                `vpod: no bytes were supplied for core module '${name}' (got ${supplied}). ` +
                    `A component needs every core module it was transpiled into, not just the largest.`,
            );
        }
        return WebAssembly.compile(source);
    };
}

export interface ComponentModule<T> {
    instantiate(
        getCoreModule: CoreModuleLoader | undefined,
        imports: Record<string, unknown>,
    ): Promise<T>;
}
