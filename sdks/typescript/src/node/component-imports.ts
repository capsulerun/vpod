import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

import * as cli from "@bytecodealliance/preview2-shim/cli";
import * as clocks from "@bytecodealliance/preview2-shim/clocks";
import * as filesystem from "@bytecodealliance/preview2-shim/filesystem";
import * as io from "@bytecodealliance/preview2-shim/io";
import * as random from "@bytecodealliance/preview2-shim/random";
import * as sockets from "@bytecodealliance/preview2-shim/sockets";

import { ipNameLookup } from "./host-resolver.js";

const WASI_PACKAGES = { cli, clocks, filesystem, io, random, sockets };

const kebabCase = (name: string) =>
    name.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`);

export const componentImports: Record<string, unknown> = {
    ...Object.fromEntries(
        Object.entries(WASI_PACKAGES).flatMap(([packageName, namespace]) =>
            Object.entries(namespace)
                .filter(([interfaceName]) => !interfaceName.startsWith("_"))
                .map(([interfaceName, value]) => [
                    `wasi:${packageName}/${kebabCase(interfaceName)}`,
                    value,
                ]),
        ),
    ),
    "../node/host-resolver.js": { ipNameLookup },
};

const compiledCoreModules = new Map<string, Promise<WebAssembly.Module>>();

export async function loadCoreModule(
    name: string,
    componentUrl: URL,
): Promise<WebAssembly.Module> {
    const directories = [new URL(".", componentUrl), new URL("../component/", componentUrl)];

    for (const directory of directories) {
        const path = fileURLToPath(new URL(name, directory));
        if (!existsSync(path)) {
            continue;
        }

        let compiled = compiledCoreModules.get(path);
        if (compiled === undefined) {
            compiled = readFile(path).then((bytes) => WebAssembly.compile(bytes));
            compiledCoreModules.set(path, compiled);
            compiled.catch(() => compiledCoreModules.delete(path));
        }
        return compiled;
    }

    throw new Error(
        `vpod: core module '${name}' is missing from ${directories.map(String).join(" and ")}`,
    );
}
