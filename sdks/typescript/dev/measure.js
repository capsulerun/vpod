

import { summarize, timeGuest, workload } from "./measure-workload.js";

const { buildId } = await (await fetch("/build-id")).json();
const { Sandbox, SandboxRuntime, networkAvailability } = await import(
    `/b/${buildId}/dist/index.js`
);

const parameters = new URLSearchParams(location.search);
const SNAPSHOT_NAME = parameters.get("name") ?? "vsnap-base:latest";
const REGISTRY_URL = parameters.get("registry") ?? "/registry/snapshots.json";
const APK_MIRROR = parameters.get("apk");

const lines = [];
const shown = [];

function show(line) {
    shown.push(line);
    console.log(line);
    document.getElementById("out").textContent = shown.join("\n");
}

// `say` is echoed by the runner;
function say(line) {
    lines.push(line);
    show(line);
}

const rows = [];
const skipped = [];

try {
    const availability = networkAvailability();
    say(`crossOriginIsolated=${crossOriginIsolated} network=${availability.available} ${availability.reason ?? ""}`);
    if (!availability.available) {
        throw new Error(availability.reason);
    }

    say("\n── floor (SDK overhead) ─────────────────────────────────────────");

    const primer = new SandboxRuntime({});
    await primer.ready();
    const pulled = await primer.pullSnapshot(SNAPSHOT_NAME, { registryUrl: REGISTRY_URL });
    say(
        `    ${"snapshot pull".padEnd(30)} guest ${((pulled.fetchMilliseconds + pulled.verifyMilliseconds + pulled.storeMilliseconds) / 1000).toFixed(3)}s   ` +
            `source=${pulled.source} fetch=${(pulled.fetchMilliseconds / 1000).toFixed(3)}s ` +
            `verify=${(pulled.verifyMilliseconds / 1000).toFixed(3)}s store=${(pulled.storeMilliseconds / 1000).toFixed(3)}s`,
    );

    let startedAt = performance.now();
    const box = await Sandbox.create({ snapshot: SNAPSHOT_NAME, registryUrl: REGISTRY_URL });
    rows.push({
        label: "Sandbox.create() warm cache",
        guestSeconds: (performance.now() - startedAt) / 1000,
        nativeSeconds: null,
    });
    say(`    ${"Sandbox.create()".padEnd(30)} guest ${rows.at(-1).guestSeconds.toFixed(3)}s   lazy, no restore yet`);

    startedAt = performance.now();
    const first = await box.commands.run("echo ready", { timeout: 120 });
    if (first.exitCode !== 0) {
        throw new Error(`the sandbox never came up: ${first.stderr}`);
    }
    rows.push({
        label: "first command (guest restore)",
        guestSeconds: (performance.now() - startedAt) / 1000,
        nativeSeconds: null,
    });
    say(`    ${"first command".padEnd(30)} guest ${rows.at(-1).guestSeconds.toFixed(3)}s   real cold start`);

    await box.commands.run("python3 -c pass", { timeout: 120 });
    await box.commands.run("echo warmup", { timeout: 60 });

    if (APK_MIRROR) {
        await box.commands.run(
            `printf '%s/main\\n%s/community\\n' '${APK_MIRROR}' '${APK_MIRROR}' > /etc/apk/repositories`,
            { timeout: 60 },
        );
    }

    let group = "floor";
    for (const step of workload()) {
        if (step.needs === "apk" && !APK_MIRROR) {
            skipped.push(step.label);
            say(`    ${step.label.padEnd(30)} skipped (no CORS-open mirror given)`);
            continue;
        }

        if (step.group !== group) {
            group = step.group;
            say(`\n── ${group} ────────────────────────────────────────────────────`);
        }

        try {
            const guestSeconds = await timeGuest(box, step, say);
            rows.push({ label: step.label, guestSeconds, nativeSeconds: null, native: step.group });
        } catch (failure) {
            say(`    !! FAILED ${step.label}: ${failure.message}`);
        }
    }

    show(summarize(rows));
    await fetch("/result", { method: "POST", body: JSON.stringify({ rows, skipped, lines }) });
} catch (thrown) {
    say(`\nFAILED: ${thrown?.stack ?? String(thrown)}`);
    await fetch("/result", {
        method: "POST",
        body: JSON.stringify({ failed: true, error: String(thrown), rows, skipped, lines }),
    });
}
