import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, describe, it } from "node:test";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const distMissing = !existsSync(join(packageRoot, "dist", "index.js"));


const NOT_OUR_BUNDLE = /^dist\/(component|component-node|node)\//;

function packedFiles() {
    const output = execFileSync("npm", ["pack", "--dry-run", "--json"], {
        cwd: packageRoot,
        encoding: "utf8",
        stdio: ["ignore", "pipe", "ignore"],
    });
    return JSON.parse(output)[0].files.map((file) => file.path);
}

describe("package exports", { skip: distMissing ? "dist/ is missing: run npm run build" : false }, () => {
    let workspace;
    let files;

    before(() => {
        files = packedFiles();

        workspace = mkdtempSync(join(tmpdir(), "vpod-exports-"));
        mkdirSync(join(workspace, "node_modules", "@capsule-run"), { recursive: true });
        symlinkSync(packageRoot, join(workspace, "node_modules", "@capsule-run", "vpod"), "dir");
    });

    after(() => {
        if (workspace !== undefined) {
            rmSync(workspace, { recursive: true, force: true });
        }
    });

    function probe(source) {
        const script = join(workspace, `probe-${Math.random().toString(36).slice(2)}.mjs`);
        writeFileSync(script, source);
        const output = execFileSync(process.execPath, [script], {
            cwd: workspace,
            encoding: "utf8",
        });
        return JSON.parse(output);
    }

    it("resolves the node subpath to the Node build", () => {
        const result = probe(`
            const specifier = "@capsule-run/vpod/node";
            const module = await import(specifier);
            console.log(JSON.stringify({
                url: import.meta.resolve(specifier),
                exports: Object.keys(module).sort(),
            }));
        `);

        assert.match(result.url, /dist\/node\/index\.js$/);
        for (const name of [
            "Sandbox",
            "createNodeTransport",
            "createNodeWorkerTransport",
            "defaultCacheDirectory",
            "FileSnapshotStore",
        ]) {
            assert.ok(result.exports.includes(name), `${name} is not exported from vpod/node`);
        }
    });

    it("leaves the bare specifier on the browser build", () => {
        const result = probe(`
            const specifier = "@capsule-run/vpod";
            const module = await import(specifier);
            console.log(JSON.stringify({
                url: import.meta.resolve(specifier),
                exports: Object.keys(module).sort(),
            }));
        `);

        assert.match(result.url, /dist\/index\.js$/);
        assert.ok(result.exports.includes("Sandbox"));
        assert.ok(
            !result.exports.includes("createNodeTransport"),
            "the browser entry is leaking the Node transport",
        );
    });

    it("ships both builds in the tarball", () => {
        for (const path of [
            "dist/index.js",
            "dist/component/vpod.js",
            "dist/node/index.js",
            "dist/node/worker-entry.js",
            "dist/component-node/vpod.js",
        ]) {
            assert.ok(files.includes(path), `${path} would not be published`);
        }

        assert.ok(
            files.some((path) => path === "dist/net/entry.js"),
            "the fetch driver's worker would not be published",
        );
    });

    it("keeps the browser bundle free of Node builtins", () => {
        const ours = files.filter(
            (path) => path.startsWith("dist/") && path.endsWith(".js") && !NOT_OUR_BUNDLE.test(path),
        );
        assert.ok(ours.length > 5, `expected a browser bundle, found ${ours.length} files`);

        const offenders = ours.filter((path) =>
            /["']node:[a-z_/]+["']/.test(readFileSync(join(packageRoot, path), "utf8")),
        );
        assert.deepEqual(offenders, [], "the browser bundle imports a Node builtin");
    });
});
