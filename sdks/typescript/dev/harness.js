const { buildId } = await (await fetch("/build-id")).json();
const { SandboxRuntime, Sandbox } = await import(`/b/${buildId}/dist/index.js`);

const parameters = new URLSearchParams(location.search);
const SNAPSHOT_NAME = parameters.get("name") ?? "vsnap-base:latest";
const REGISTRY_URL = parameters.get("registry") ?? "/registry/snapshots.json";
const RUN_COUNT = Number(parameters.get("runs") ?? 3);

const AB_LOOP =
    "s=0\n" +
    "for i in range(1000000): s=(s+i*i)^(i&0xff)\n" +
    "print(s)\n";
const AB_LOOP_EXPECTED = "333332833346999360";

const PYTHON = String.fromCharCode(0);

const lines = [];
function say(line) {
    lines.push(line);
    console.log(line);
    document.getElementById("out").textContent = lines.join("\n");
}

function startMainThreadTicker() {
    let previous = performance.now();
    let worstGapMilliseconds = 0;
    let frames = 0;
    let running = true;
    const ticker = document.getElementById("ticker");

    function tick(now) {
        if (!running) return;
        const gap = now - previous;
        previous = now;
        frames++;
        if (gap > worstGapMilliseconds) worstGapMilliseconds = gap;
        ticker.textContent = `main thread ticker: ${frames} frames, worst gap ${worstGapMilliseconds.toFixed(0)}ms`;
        requestAnimationFrame(tick);
    }
    requestAnimationFrame(tick);

    return {
        stop() {
            running = false;
            return { worstGapMilliseconds, frames };
        },
    };
}

async function memoryReport() {
    const report = {};

    if (performance.memory) {
        report.usedJsHeapBytes = performance.memory.usedJSHeapSize;
        report.totalJsHeapBytes = performance.memory.totalJSHeapSize;
    }

    if (crossOriginIsolated && performance.measureUserAgentSpecificMemory) {
        try {
            const measured = await performance.measureUserAgentSpecificMemory();
            report.userAgentSpecificBytes = measured.bytes;
        } catch (error) {
            report.userAgentSpecificError = String(error);
        }
    }

    return report;
}

async function suspendResumeRoundTrip() {
    const report = {};
    const sandbox = await Sandbox.create({
        snapshot: SNAPSHOT_NAME,
        registryUrl: REGISTRY_URL,
    });

    try {
        await sandbox.commands.run("export MARKER=survived");
        await sandbox.commands.run("echo filedata > /tmp/keep.txt");

        let startedAt = performance.now();
        const instanceId = await sandbox.suspendToOpfs();
        report.suspendMilliseconds = performance.now() - startedAt;
        report.instances = (await Sandbox.listInstances()).length;
        await sandbox.close();

        startedAt = performance.now();
        const resumed = await Sandbox.resume(instanceId, {
            snapshot: SNAPSHOT_NAME,
            registryUrl: REGISTRY_URL,
        });
        report.resumeMilliseconds = performance.now() - startedAt;

        try {
            report.environmentSurvived =
                (await resumed.commands.run("echo $MARKER")).stdout.trim() === "survived";
            report.fileSurvived =
                (await resumed.commands.run("cat /tmp/keep.txt")).stdout.trim() === "filedata";
        } finally {
            await resumed.close();
        }

        report.instancesAfterResume = (await Sandbox.listInstances()).length;
        report.ok = report.environmentSurvived && report.fileSurvived;
    } catch (error) {
        report.ok = false;
        report.error = String(error && error.stack ? error.stack : error);
        await sandbox.close().catch(() => {});
    }

    return report;
}

async function main() {
    say(`ua          : ${navigator.userAgent}`);
    say(`isolated    : ${crossOriginIsolated}`);

    const manifest = await (await fetch("/dist/component/manifest.json")).json();
    say(`tier        : ${manifest.tier} (${manifest.source})`);
    say(`core wasm   : ${(manifest.coreWasmBytes / 1048576).toFixed(1)} MiB`);
    say("");

    const ticker = startMainThreadTicker();
    const runtime = new SandboxRuntime();

    const componentLoadMilliseconds = await runtime.ready();
    say(`boot: compile+instantiate = ${(componentLoadMilliseconds / 1000).toFixed(3)}s`);

    const quotaBefore = await runtime.storageQuota();

    const pullStartedAt = performance.now();
    const mount = await runtime.pullSnapshot(SNAPSHOT_NAME, { registryUrl: REGISTRY_URL });
    const fetchMilliseconds = performance.now() - pullStartedAt;
    say(`boot: pull snapshot       = ${(fetchMilliseconds / 1000).toFixed(3)}s  (${(mount.byteLength / 1048576).toFixed(1)} MiB, from ${mount.source})`);
    say(
        `      acquire ${(mount.fetchMilliseconds / 1000).toFixed(3)}s  verify ${(mount.verifyMilliseconds / 1000).toFixed(3)}s  store ${(mount.storeMilliseconds / 1000).toFixed(3)}s`,
    );

    const startStartedAt = performance.now();
    const handle = await runtime.sessionStart(mount.snapshotPath);
    const startMilliseconds = performance.now() - startStartedAt;
    say(`boot: session-start       = ${(startMilliseconds / 1000).toFixed(3)}s`);

    const secondStartedAt = performance.now();
    const secondHandle = await runtime.sessionStart(mount.snapshotPath);
    const secondStartMilliseconds = performance.now() - secondStartedAt;
    say(`boot: 2nd session-start   = ${(secondStartMilliseconds / 1000).toFixed(3)}s  (base cache)`);
    await runtime.sessionClose(secondHandle);

    const warmStartedAt = performance.now();
    const warm = await runtime.sessionExec(handle, `${PYTHON}print('warm')`, 120n);
    const warmMilliseconds = performance.now() - warmStartedAt;
    say(`boot: first exec (warm)   = ${(warmMilliseconds / 1000).toFixed(3)}s  out=${JSON.stringify(warm.stdout.trim())}`);
    say(
        `boot: total to usable     = ${((componentLoadMilliseconds + startMilliseconds + warmMilliseconds) / 1000).toFixed(3)}s  (excl. fetch)`,
    );
    say("");

    const durations = [];
    let correct = true;
    for (let run = 0; run < RUN_COUNT; run++) {
        const startedAt = performance.now();
        const result = await runtime.sessionExec(handle, PYTHON + AB_LOOP, 120n);
        const seconds = (performance.now() - startedAt) / 1000;
        durations.push(seconds);
        const output = result.stdout.trim();
        if (output !== AB_LOOP_EXPECTED) correct = false;
        say(`ab_loop run ${run}: ${seconds.toFixed(3)}s   out=${JSON.stringify(output)}`);
    }

    const sorted = [...durations].sort((a, b) => a - b);
    const median = sorted[Math.floor(sorted.length / 2)];
    say("");
    say(`ab_loop median: ${median.toFixed(3)}s`);
    say(`output correct: ${correct}`);

    const { spinCount, spinNanoseconds } = await runtime.pollStats();
    say(`poll spins    : ${spinCount} totalling ${(spinNanoseconds / 1e9).toFixed(3)}s`);

    const responsiveness = ticker.stop();
    say(
        `main thread   : ${responsiveness.frames} frames, worst gap ${responsiveness.worstGapMilliseconds.toFixed(0)}ms`,
    );

    const memory = await memoryReport();
    for (const [key, value] of Object.entries(memory)) {
        say(`memory.${key} = ${typeof value === "number" ? (value / 1048576).toFixed(1) + " MiB" : value}`);
    }

    const quotaAfter = await runtime.storageQuota();
    if (quotaAfter) {
        const megabytes = (bytes) => `${(bytes / 1048576).toFixed(0)} MiB`;
        say(
            `opfs          : ${megabytes(quotaBefore?.usage ?? 0)} -> ${megabytes(quotaAfter.usage)} used of ${megabytes(quotaAfter.quota)} quota`,
        );
    }

    await runtime.sessionClose(handle);
    runtime.terminate();

    const suspendResume = await suspendResumeRoundTrip();
    say("");
    say(
        `suspend/resume: ${suspendResume.ok ? "ok" : "FAILED"}` +
            (suspendResume.ok
                ? `  suspend ${(suspendResume.suspendMilliseconds / 1000).toFixed(3)}s  resume ${(suspendResume.resumeMilliseconds / 1000).toFixed(3)}s  (opfs instances ${suspendResume.instances} -> ${suspendResume.instancesAfterResume})`
                : `  ${suspendResume.error}`),
    );

    return {
        suspendResume,
        userAgent: navigator.userAgent,
        crossOriginIsolated,
        tier: manifest.tier,
        coreWasmBytes: manifest.coreWasmBytes,
        componentLoadMilliseconds,
        fetchMilliseconds,
        startMilliseconds,
        secondStartMilliseconds,
        warmMilliseconds,
        snapshotBytes: mount.byteLength,
        snapshotId: mount.id,
        snapshotSource: mount.source,
        pullAcquireMilliseconds: mount.fetchMilliseconds,
        pullVerifyMilliseconds: mount.verifyMilliseconds,
        pullStoreMilliseconds: mount.storeMilliseconds,
        opfsUsageBefore: quotaBefore?.usage ?? null,
        opfsUsageAfter: quotaAfter?.usage ?? null,
        opfsQuota: quotaAfter?.quota ?? null,
        abLoopSeconds: durations,
        abLoopMedianSeconds: median,
        outputCorrect: correct,
        pollSpinCount: spinCount,
        pollSpinNanoseconds: spinNanoseconds,
        mainThread: responsiveness,
        memory,
        log: lines.join("\n"),
    };
}

main()
    .then((report) => {
        say("\nDONE");
        return fetch("/result", {
            method: "POST",
            body: JSON.stringify(report, null, 2),
        }).catch(() => {});
    })
    .catch((error) => {
        const text = `${lines.join("\n")}\n\nFAILED: ${error && error.stack ? error.stack : error}`;
        document.getElementById("out").textContent = text;
        return fetch("/result", {
            method: "POST",
            body: JSON.stringify({ failed: true, error: String(error), log: text }, null, 2),
        }).catch(() => {});
    });
