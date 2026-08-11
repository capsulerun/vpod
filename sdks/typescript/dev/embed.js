const { buildId } = await (await fetch("/build-id")).json();

const parameters = new URLSearchParams(location.search);
const SNAPSHOT_NAME = parameters.get("name") ?? "vsnap-base-256mb.snap";
const CORE_MODULE_NAMES = [
    "vpod.core.wasm",
    "vpod.core2.wasm",
    "vpod.core3.wasm",
    "vpod.core4.wasm",
];

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

const blobUrlFor = async (path) => {
    const source = await (await fetch(path)).text();
    return {
        url: URL.createObjectURL(new Blob([source], { type: "text/javascript" })),
        byteLength: source.length,
    };
};

const report = { crossOriginIsolated, checks, failed: false };

try {
    const asset = (path) => `/b/${buildId}${path}`;

    const main = await blobUrlFor(asset("/dist/embed/vpod.js"));
    const worker = await blobUrlFor(asset("/dist/embed/vpod.worker.js"));
    say(`embed entry ${main.byteLength} bytes, worker ${worker.byteLength} bytes, both blob:`);

    // The SDK itself is imported from a blob, so it cannot resolve anything relative to
    // its own URL. Anything it needs has to be handed to it.
    const { Sandbox, WorkerTransport } = await import(main.url);
    check("the embed entry imports from a blob: URL", true);

    const coreModules = {};
    for (const name of CORE_MODULE_NAMES) {
        coreModules[name] = await (await fetch(asset(`/dist/component/${name}`))).arrayBuffer();
    }
    const totalBytes = Object.values(coreModules).reduce((sum, b) => sum + b.byteLength, 0);
    say(`core wasm as bytes: ${(totalBytes / 1048576).toFixed(1)} MiB across ${CORE_MODULE_NAMES.length} modules`);

    const snapshotBytes = await (await fetch(`/snapshot/${SNAPSHOT_NAME}`)).arrayBuffer();
    say(`snapshot ${(snapshotBytes.byteLength / 1048576).toFixed(0)} MiB as bytes`);

    const startedAt = performance.now();
    const sandbox = await Sandbox.create({
        transport: new WorkerTransport({ workerUrl: worker.url, coreModules }),
        snapshot: { bytes: new Uint8Array(snapshotBytes), name: SNAPSHOT_NAME },
    });
    const bootMilliseconds = performance.now() - startedAt;
    report.bootMilliseconds = bootMilliseconds;
    check("a sandbox starts from blobs and bytes alone", true, `${bootMilliseconds.toFixed(0)}ms`);

    const hello = await sandbox.commands.run("echo hello");
    check("a command runs", hello.stdout.trim() === "hello", JSON.stringify(hello.stdout.trim()));

    const python = await sandbox.commands.run("python3 --version");
    check("the guest is real", python.stdout.trim().startsWith("Python"), python.stdout.trim());

    // A subshell: a bare `exit` would take the session's shell down with it.
    const exitCode = await sandbox.commands.run("sh -c 'exit 7'");
    check("exit codes come back", exitCode.exitCode === 7, `exit ${exitCode.exitCode}`);

    // Interrupt reaches a worker that was itself loaded from a blob.
    const interrupted = sandbox.commands.run("sleep 300", { timeout: 60 });
    setTimeout(() => sandbox.commands.interrupt(), 500);
    const stopped = await interrupted;
    check("an interrupt reaches the blob-loaded worker", stopped.exitCode === 130, `exit ${stopped.exitCode}`);

    await sandbox.close();

    // Control. The default worker keeps relative chunk imports, which cannot resolve
    // against a blob: base. If this ever loads, the test above has stopped proving
    // anything and the embed build is no longer doing any work.
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
