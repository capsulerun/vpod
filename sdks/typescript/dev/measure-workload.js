export const NET_REPEATS = 3;

const CORPUS = `
import os
os.makedirs(root, exist_ok=True)
for f in range(120):
    with open(os.path.join(root, "file%d.txt" % f), "w") as fh:
        for l in range(400):
            fh.write("line %d of file %d lorem ipsum dolor sit amet consectetur\\n" % (l, f))
        fh.write("NEEDLE_MARKER_XYZ\\n")
print("corpus ready")
`;

export const HOST_CORPUS_SCRIPT = (root) => `root = ${JSON.stringify(root)}\n${CORPUS}`;

export class StepFailed extends Error {}

async function shell(box, command, timeout = 300) {
    const result = await box.commands.run(command, { timeout });
    if (result.exitCode !== 0) {
        const tail = (result.stderr || result.stdout || "").trim().split("\n").slice(-4);
        throw new StepFailed(`exit ${result.exitCode}: ${command}\n       ${tail.join("\n       ")}`);
    }
    return result;
}

export function workload() {
    return [
        {
            group: "floor",
            label: "code.run('pass') warm",
            guest: (box) => box.code.run("pass"),
        },
        {
            group: "floor",
            label: "commands.run('echo hi')",
            note: "fixed round-trip",
            guest: (box) => shell(box, "echo hi"),
        },
        {
            group: "floor",
            label: "python3 -c pass",
            guest: (box) => shell(box, "python3 -c pass"),
            native: () => ["python3", "-c", "pass"],
        },

        {
            group: "apk",
            label: "apk update",
            note: "network",
            needs: "apk",
            guest: (box) => shell(box, "apk update"),
        },
        {
            group: "apk",
            label: "apk add ripgrep",
            note: "network",
            needs: "apk",
            guest: (box) => shell(box, "apk add ripgrep"),
        },

        {
            group: "uv",
            label: "uv venv",
            guest: (box) => shell(box, "rm -rf /tmp/bench-venv && uv venv /tmp/bench-venv"),
            native: (tmp) => ["uv", "venv", `${tmp}/venv`],
        },
        {
            group: "uv",
            label: "uv pip install six",
            note: "network",
            guest: (box) =>
                shell(
                    box,
                    "rm -rf /tmp/bench-tgt && uv pip install --no-cache --target /tmp/bench-tgt six",
                ),
            native: (tmp) => [
                "uv", "pip", "install", "--no-cache", "--target", `${tmp}/tgt`, "six",
            ],
        },

        {
            group: "ripgrep",
            label: "rg over 120 files / 48K lines",
            needs: "apk",
            setup: async (box) => {
                const built = await box.code.run(`root = "/tmp/bench-corpus"\n${CORPUS}`, {
                    timeout: 300,
                });
                if (!built.success) {
                    throw new StepFailed(`guest corpus generation failed: ${built.error}`);
                }
            },
            guest: (box) => shell(box, "rg -c NEEDLE_MARKER_XYZ /tmp/bench-corpus | wc -l"),
            native: (tmp) => ["rg", "-c", "NEEDLE_MARKER_XYZ", `${tmp}/corpus`],
        },

        {
            group: "wget",
            label: "wget http",
            note: "network",
            repeats: NET_REPEATS,
            guest: (box) => shell(box, "wget -q -O /dev/null http://ip-api.com/json", 60),
            native: () => ["wget", "-q", "-O", "/dev/null", "http://ip-api.com/json"],
        },
        {
            group: "wget",
            label: "wget https (TLS)",
            note: "network",
            repeats: NET_REPEATS,
            guest: (box) => shell(box, "wget -q -O /dev/null https://pypi.org/pypi/six/json", 60),
            native: () => ["wget", "-q", "-O", "/dev/null", "https://pypi.org/pypi/six/json"],
        },
    ];
}

const median = (values) => {
    const sorted = [...values].sort((a, b) => a - b);
    const middle = Math.floor(sorted.length / 2);
    return sorted.length % 2 === 0
        ? (sorted[middle - 1] + sorted[middle]) / 2
        : sorted[middle];
};

/** Times `step.guest`, discarding the run entirely if it did not succeed. */
export async function timeGuest(box, step, say) {
    if (step.setup) {
        await step.setup(box);
    }

    const times = [];
    for (let attempt = 0; attempt < (step.repeats ?? 1); attempt++) {
        const startedAt = performance.now();
        await step.guest(box);
        times.push((performance.now() - startedAt) / 1000);
    }

    const seconds = median(times);
    say(`    ${step.label.padEnd(30)} guest ${seconds.toFixed(3)}s   ${step.note ?? ""}`);
    return seconds;
}

export function summarize(rows) {
    const lines = [
        "",
        "══ summary ══════════════════════════════════════════════════════",
        `${"workload".padEnd(32)}${"guest".padStart(9)}${"native".padStart(10)}${"ratio".padStart(9)}`,
        "─".repeat(62),
    ];

    const ratios = [];
    for (const row of rows) {
        const native = row.nativeSeconds ? `${row.nativeSeconds.toFixed(3)}s`.padStart(10) : "".padStart(10);
        let ratio = "".padStart(9);
        if (row.nativeSeconds) {
            const value = row.guestSeconds / row.nativeSeconds;
            ratios.push([row.label, value]);
            ratio = `${value.toFixed(1)}x`.padStart(9);
        }
        lines.push(
            `${row.label.padEnd(32)}${`${row.guestSeconds.toFixed(3)}s`.padStart(9)}${native}${ratio}`,
        );
    }

    if (ratios.length > 0) {
        const best = ratios.reduce((a, b) => (a[1] <= b[1] ? a : b));
        const worst = ratios.reduce((a, b) => (a[1] >= b[1] ? a : b));
        lines.push("─".repeat(62));
        lines.push(`spread: ${best[1].toFixed(1)}x (${best[0]}) .. ${worst[1].toFixed(1)}x (${worst[0]})`);
        lines.push("");
        lines.push("The spread is the finding. Quote the table, not one multiplier.");
    }

    return lines.join("\n");
}
