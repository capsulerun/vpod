const { buildId } = await (await fetch("/build-id")).json();

const parameters = new URLSearchParams(location.search);
const SNAPSHOT_NAME = parameters.get("name") ?? "vsnap-base-256mb.snap";
const SNAPSHOT_ID = SNAPSHOT_NAME.replace(/\.snap$/, "");

const lines = [];
function say(line) {
    lines.push(line);
    console.log(line);
    document.getElementById("out").textContent = lines.join("\n");
}

const checks = [];
function check(name, passed, detail) {
    checks.push({ name, passed, detail });
    say(`${passed ? "ok  " : "FAIL"} ${name}${detail ? `  ${detail}` : ""}`);
}

const sourceOf = (path) => fetch(path).then((response) => response.text());

const blobUrlFor = async (path) => {
    const source = await sourceOf(path);
    return {
        url: URL.createObjectURL(new Blob([source], { type: "text/javascript" })),
        byteLength: source.length,
    };
};

const report = { crossOriginIsolated, checks, failed: false };

try {
    const asset = (path) => `/b/${buildId}${path}`;

    const main = await blobUrlFor(asset("/dist/embed/vpod.js"));
    say(`embed entry ${main.byteLength} bytes, one file`);

    const { Sandbox } = await import(main.url);
    check("the embed entry imports from a blob: URL", true);

    const coreModules = await (await fetch(asset("/dist/component/vpod.core.wasm"))).arrayBuffer();
    say(`core wasm as bytes: ${(coreModules.byteLength / 1048576).toFixed(1)} MiB, one module`);

    const snapshotBytes = await (await fetch(`/snapshot/${SNAPSHOT_NAME}`)).arrayBuffer();
    say(`snapshot ${(snapshotBytes.byteLength / 1048576).toFixed(0)} MiB as bytes`);

    const startedAt = performance.now();
    const sandbox = await Sandbox.create({
        coreModules,
        snapshot: { bytes: new Uint8Array(snapshotBytes), name: SNAPSHOT_NAME },
    });

    const bootMilliseconds = performance.now() - startedAt;
    report.bootMilliseconds = bootMilliseconds;
    check("a sandbox starts from blobs and bytes alone", true, `${bootMilliseconds.toFixed(0)}ms`);

    const hello = await sandbox.commands.run("echo hello");
    check("a command runs", hello.stdout.trim() === "hello", JSON.stringify(hello.stdout.trim()));

    const python = await sandbox.commands.run("python3 --version");
    check("the guest is real", python.stdout.trim().startsWith("Python"), python.stdout.trim());

    const exitCode = await sandbox.commands.run("sh -c 'exit 7'");
    check("exit codes come back", exitCode.exitCode === 7, `exit ${exitCode.exitCode}`);

    const interrupted = sandbox.commands.run("sleep 300", { timeout: 60 });
    setTimeout(() => sandbox.commands.interrupt(), 500);
    const stopped = await interrupted;
    check("an interrupt reaches the blob-loaded worker", stopped.exitCode === 130, `exit ${stopped.exitCode}`);

    await sandbox.close();


    {
        // Absolute: a blob-loaded worker has no base to resolve a relative URL against.
        const pulled = await Sandbox.create({
            coreModules,
            snapshot: SNAPSHOT_ID,
            registryUrl: new URL("/registry/snapshots.json", location.origin).href,
        });
        const result = await pulled.commands.run("echo pulled");
        check(
            "a snapshot pulls by name from a registry",
            result.stdout.trim() === "pulled",
            JSON.stringify(result.stdout.trim()),
        );
        await pulled.close();
    }

    const control = await blobUrlFor(asset("/dist/worker/entry.js"));
    const controlOutcome = await new Promise((resolve) => {
        const controlWorker = new Worker(control.url, { type: "module" });
        controlWorker.addEventListener("error", () => resolve("failed to load"));
        controlWorker.addEventListener("message", () => resolve("loaded"));
        controlWorker.postMessage({ kind: "init", componentUrl: null });
        setTimeout(() => resolve("failed to load"), 5000);
    });
    check(
        "the default chunk-split worker still cannot load from a blob",
        controlOutcome === "failed to load",
        controlOutcome,
    );
} catch (thrown) {
    report.failed = true;
    report.error = String(thrown?.stack ?? thrown);
    say(`FAILED: ${report.error}`);
}

report.passed = !report.failed && checks.every((entry) => entry.passed);
await fetch("/result", { method: "POST", body: JSON.stringify(report, null, 2) });
