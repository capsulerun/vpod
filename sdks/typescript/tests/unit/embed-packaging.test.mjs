import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, it } from "node:test";

import { relativeImportsIn } from "../../scripts/build.mjs";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const distDir = join(packageRoot, "dist");
const embedDir = join(distDir, "embed");

const EMBED_FILES = ["vpod.js", "vpod.worker.js"];

const sourceOf = (path) => readFileSync(join(distDir, path), "utf8");

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

    it("keeps Node builtins out of the worker, which bundles the component glue", () => {
        assert.doesNotMatch(sourceOf(join("embed", "vpod.worker.js")), /["']node:[a-z_/]+["']/);
    });

    it("does not inline the core wasm, which the embedder supplies as bytes", () => {
        for (const name of EMBED_FILES) {
            const bytes = readFileSync(join(embedDir, name)).byteLength;
            assert.ok(
                bytes < 8 * 1024 * 1024,
                `dist/embed/${name} is ${bytes} bytes, large enough to suggest the wasm got inlined`,
            );
        }
    });
});
