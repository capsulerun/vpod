const { buildId } = await (await fetch("/build-id")).json();
const { Sandbox } = await import(`/b/${buildId}/dist/index.js`);

const parameters = new URLSearchParams(location.search);
const IDS = (parameters.get("ids") ?? "").split(",").filter((id) => id !== "");
const REGISTRY_URL = parameters.get("registry") ?? "/registry/snapshots.json";

const lines = [];
function say(line) {
    lines.push(line);
    console.log(line);
    document.getElementById("out").textContent = lines.join("\n");
}

async function guestMemoryTotalMb(id) {
    const sandbox = await Sandbox.create({ snapshot: id, registryUrl: REGISTRY_URL });
    try {
        const result = await sandbox.commands.run("free -m | awk '/^Mem:/ { print $2 }'");
        if (result.exitCode !== 0) {
            throw new Error(`free -m exited ${result.exitCode}: ${result.stderr}`);
        }

        const total = Number(result.stdout.trim());
        if (!Number.isFinite(total) || total <= 0) {
            throw new Error(`free -m printed ${JSON.stringify(result.stdout)}`);
        }
        return total;
    } finally {
        await sandbox.close();
    }
}

const report = { totals: {}, crossOriginIsolated: self.crossOriginIsolated };

try {
    if (IDS.length === 0) {
        throw new Error("no ids given: pass ?ids=a,b,c");
    }

    for (const id of IDS) {
        const startedAt = performance.now();
        const total = await guestMemoryTotalMb(id);
        report.totals[id] = total;
        say(`${id}  ${total} MB  (${((performance.now() - startedAt) / 1000).toFixed(1)}s)`);
    }
} catch (thrown) {
    report.failed = true;
    report.error = String(thrown?.stack ?? thrown);
    say(`FAILED ${report.error}`);
}

await fetch("/result", { method: "POST", body: JSON.stringify(report) });
