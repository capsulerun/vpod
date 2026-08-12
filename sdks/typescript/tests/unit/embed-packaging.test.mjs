import assert from "node:assert/strict";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, it } from "node:test";

import { relativeImportsIn } from "../../scripts/build.mjs";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const distDir = join(packageRoot, "dist");
const embedDir = join(distDir, "embed");
const componentDir = join(distDir, "component");

const EMBED_FILES = ["vpod.js"];
const INLINE_LIMIT = 64 * 1024;

const sourceOf = (path) => readFileSync(join(distDir, path), "utf8");
const base64Of = (name) => readFileSync(join(componentDir, name)).toString("base64");

describe("embed packaging", () => {
    it("emits one self-contained file per entry point", () => {
        for (const name of EMBED_FILES) {
            assert.ok(existsSync(join(embedDir, name)), `dist/embed/${name} is missing`);
        }
    });

    it("keeps relative imports out, since a blob: URL cannot resolve them", () => {
        for (const name of EMBED_FILES) {
            assert.deepEqual(
                relativeImportsIn(sourceOf(join("embed", name))),
                [],
                `dist/embed/${name} keeps relative imports`,
            );
        }
    });

    it("finds the relative imports in the default build, so the check above can fail", () => {
        assert.ok(
            relativeImportsIn(sourceOf("worker/entry.js")).length > 0,
            "expected the chunk-split worker to have relative imports",
        );
    });

    it("keeps Node builtins out of the entry, which bundles the component glue", () => {
        assert.doesNotMatch(sourceOf(join("embed", "vpod.js")), /["']node:[a-z_/]+["']/);
    });

    it("bakes in the small core modules, so a caller passes one buffer", () => {
        const source = sourceOf(join("embed", "vpod.js"));

        const small = readdirSync(componentDir)
            .filter((name) => name.endsWith(".wasm"))
            .filter((name) => statSync(join(componentDir, name)).size <= INLINE_LIMIT);

        assert.ok(small.length >= 1, "expected the component to have small core modules");

        for (const name of small) {
            assert.ok(
                source.includes(base64Of(name)),
                `${name} is not baked in, so a caller would have to supply it`,
            );
        }
    });

    it("does not bake in the engine, which the caller supplies as bytes", () => {
        assert.ok(
            !sourceOf(join("embed", "vpod.js")).includes(base64Of("vpod.core.wasm")),
            "the engine is inlined into the embed entry, which defeats caching",
        );
    });

    it("stays far smaller than the engine it loads", () => {
        for (const name of EMBED_FILES) {
            const bytes = readFileSync(join(embedDir, name)).byteLength;
            assert.ok(
                bytes < 8 * 1024 * 1024,
                `dist/embed/${name} is ${bytes} bytes, large enough to suggest the wasm got inlined`,
            );
        }
    });
});
