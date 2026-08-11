const { buildId } = await (await fetch("/build-id")).json();
const { Sandbox } = await import(`/b/${buildId}/dist/index.js`);

const parameters = new URLSearchParams(location.search);
const SNAPSHOT_NAME = parameters.get("name") ?? "vsnap-base:latest";
const REGISTRY_URL = parameters.get("registry") ?? "/registry/snapshots.json";

const FOREVER = "sleep 30";

const lines = [];
function say(line) {
    lines.push(line);
    console.log(line);
    document.getElementById("out").textContent = lines.join("\n");
}

function startTicker() {
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

    return () => {
        running = false;
        return { frames, worstGapMilliseconds };
    };
}

const checks = [];
function check(name, passed, detail) {
    checks.push({ name, passed, detail });
    say(`${passed ? "ok  " : "FAIL"} ${name}${detail ? `  ${detail}` : ""}`);
}

const report = { crossOriginIsolated, checks, failed: false };
const stopTicker = startTicker();

try {
    const sandbox = await Sandbox.create({
        snapshot: SNAPSHOT_NAME,
        registryUrl: REGISTRY_URL,
    });

    await sandbox.commands.run("true");
    say(`isolated ${crossOriginIsolated}, sandbox up`);

    {
        const started = performance.now();
        const pending = sandbox.commands.run(FOREVER, { timeout: 60 });
        setTimeout(() => void sandbox.commands.interrupt(), 500);
        const result = await pending;
        const seconds = (performance.now() - started) / 1000;
        check(
            "interrupt() stops a running command with 130",
            result.exitCode === 130 && seconds < 10,
            `exit=${result.exitCode} in ${seconds.toFixed(2)}s`,
        );
    }

    {
        const pending = sandbox.commands.run(`echo working; ${FOREVER}`, { timeout: 60 });
        setTimeout(() => void sandbox.commands.interrupt(), 500);
        const result = await pending;
        check(
            "output before the stop is kept",
            result.exitCode === 130 && result.stdout.trim() === "working",
            `exit=${result.exitCode} stdout=${JSON.stringify(result.stdout.trim())}`,
        );
    }

    {
        const controller = new AbortController();
        setTimeout(() => controller.abort(), 500);
        let name = "did not reject";
        try {
            await sandbox.commands.run(FOREVER, { timeout: 60, signal: controller.signal });
        } catch (thrown) {
            name = thrown.name;
        }
        check("AbortSignal rejects with AbortError", name === "AbortError", name);
    }

    {
        let name = "did not reject";
        try {
            await sandbox.commands.run(FOREVER, { timeout: 60, signal: AbortSignal.timeout(500) });
        } catch (thrown) {
            name = thrown.name;
        }
        check("AbortSignal.timeout rejects with TimeoutError", name === "TimeoutError", name);
    }

    {
        await sandbox.commands.interrupt();
        const after = await sandbox.commands.run("echo clean");
        check(
            "interrupt while idle is a no-op",
            after.exitCode === 0 && after.stdout.trim() === "clean",
            JSON.stringify(after.stdout.trim()),
        );
    }

    {
        const after = await sandbox.commands.run("echo alive");
        check("session survives", after.stdout.trim() === "alive", JSON.stringify(after.stdout.trim()));
    }

    // The cost question, measured where the round trip is a real postMessage
    // rather than a synchronous call.
    {
        const N = 30;
        const started = performance.now();
        for (let i = 0; i < N; i++) await sandbox.commands.run(":");
        report.perCallMilliseconds = (performance.now() - started) / N;
        say(`round trip ${report.perCallMilliseconds.toFixed(2)} ms per short command`);
    }

    // A command long enough to span many slices, to see the overhead that only
    // shows up past the first one.
    {
        const started = performance.now();
        const result = await sandbox.commands.run("awk 'BEGIN{for(i=0;i<200000;i++)x+=i}'; echo done", {
            timeout: 120,
        });
        report.longCommandSeconds = (performance.now() - started) / 1000;
        check(
            "a command spanning many slices still completes",
            result.exitCode === 0 && result.stdout.trim() === "done",
            `${report.longCommandSeconds.toFixed(2)}s`,
        );
    }

    await sandbox.close();
} catch (thrown) {
    report.failed = true;
    report.error = String(thrown?.stack ?? thrown);
    say(`FAILED ${report.error}`);
}

const ticker = stopTicker();
report.frames = ticker.frames;
report.worstGapMilliseconds = ticker.worstGapMilliseconds;
report.passed = checks.every((entry) => entry.passed);
say(
    `main thread: ${ticker.frames} frames, worst gap ${ticker.worstGapMilliseconds.toFixed(0)}ms`,
);

await fetch("/result", { method: "POST", body: JSON.stringify(report) });
