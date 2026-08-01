/**
 * Quick end-to-end in browser
 */

const { buildId } = await (await fetch("/build-id")).json();
const { Sandbox, networkAvailability } = await import(`/b/${buildId}/dist/index.js`);

const parameters = new URLSearchParams(location.search);
const SNAPSHOT_NAME = parameters.get("name") ?? "vsnap-base:latest";
const REGISTRY_URL = parameters.get("registry") ?? "/registry/snapshots.json";

const lines = [];
function say(line) {
    lines.push(line);
    console.log(line);
    document.getElementById("out").textContent = lines.join("\n");
}

async function report(payload) {
    await fetch("/result", { method: "POST", body: JSON.stringify(payload) });
}

const steps = [];
async function step(name, run) {
    const startedAt = performance.now();
    try {
        const detail = await run();
        const milliseconds = performance.now() - startedAt;
        steps.push({ name, ok: true, milliseconds, detail });
        say(`ok   ${name} (${(milliseconds / 1000).toFixed(2)}s) ${detail ?? ""}`);
        return detail;
    } catch (thrown) {
        const milliseconds = performance.now() - startedAt;
        const message = thrown?.stack ?? String(thrown);
        steps.push({ name, ok: false, milliseconds, detail: message });
        say(`FAIL ${name} (${(milliseconds / 1000).toFixed(2)}s)\n${message}`);
        throw thrown;
    }
}

try {
    const availability = networkAvailability();
    say(`crossOriginIsolated=${crossOriginIsolated} network=${availability.available} ${availability.reason ?? ""}`);

    if (!availability.available) {
        throw new Error(availability.reason);
    }

    let box;
    await step("create sandbox with network", async () => {
        box = await Sandbox.create({
            snapshot: SNAPSHOT_NAME,
            registryUrl: REGISTRY_URL,
        });
        return box.snapshotId;
    });

    await step("guest resolves a hostname", async () => {
        const result = await box.commands.run("getent hosts pypi.org || nslookup pypi.org", {
            timeout: 30,
        });
        return `exit=${result.exitCode} ${result.stdout.trim().slice(0, 120)}`;
    });

    await step("guest fetches over https with wget", async () => {
        const result = await box.commands.run(
            "wget -q -O- https://pypi.org/simple/ | head -c 80",
            { timeout: 60 },
        );
        if (result.exitCode !== 0) {
            throw new Error(`wget exited ${result.exitCode}: ${result.stderr}`);
        }
        return result.stdout.trim().slice(0, 80);
    });

    await step("guest fetches over plain http", async () => {
        const result = await box.commands.run("wget -q -O- http://ip-api.com/json | head -c 60", {
            timeout: 60,
        });
        if (result.exitCode !== 0) {
            throw new Error(`wget exited ${result.exitCode}: ${result.stderr}`);
        }
        return result.stdout.trim().slice(0, 60);
    });

    await step("python reaches a CORS-friendly API", async () => {
        const code =
            "import urllib.request\n" +
            "body = urllib.request.urlopen('https://pypi.org/pypi/six/json', timeout=60).read()\n" +
            "print(len(body))\n";
        const result = await box.code.run(code, { timeout: 90 });
        if (!result.success) {
            throw new Error(`python failed: ${result.error}`);
        }
        return `${result.text.trim()} bytes`;
    });

    const apkMirror = parameters.get("apk");

    if (apkMirror) {
        await step("apk update through a CORS-open mirror", async () => {
            const result = await box.commands.run(
                `printf '%s/main\\n%s/community\\n' '${apkMirror}' '${apkMirror}'` +
                    " > /etc/apk/repositories && apk update 2>&1 | tail -3",
                { timeout: 300 },
            );
            if (result.exitCode !== 0) {
                throw new Error(`apk update exited ${result.exitCode}: ${result.stdout}`);
            }
            return result.stdout.trim().slice(-200);
        });

        await step("apk add a package", async () => {
            const result = await box.commands.run(
                "apk add --no-cache jq 2>&1 | tail -3 && jq --version",
                { timeout: 300 },
            );
            if (result.exitCode !== 0) {
                throw new Error(`apk add exited ${result.exitCode}: ${result.stdout}`);
            }
            return result.stdout.trim().slice(-200);
        });
    }

    const installer = await step("find an installer", async () => {
        const result = await box.commands.run(
            "command -v uv >/dev/null && echo uv; command -v pip >/dev/null && echo pip",
            { timeout: 30 },
        );
        return result.stdout.trim().split("\n").filter(Boolean).join(",");
    });

    if (installer.includes("uv")) {
        await step("uv install a pure wheel", async () => {
            const result = await box.commands.run(
                "uv pip install --system six 2>&1 | tail -3",
                { timeout: 240 },
            );
            if (result.exitCode !== 0) {
                throw new Error(`uv exited ${result.exitCode}: ${result.stdout}`);
            }
            return result.stdout.trim().slice(-160);
        });
    }

    if (installer.includes("pip")) {
        await step("pip install a pure wheel", async () => {
            const result = await box.commands.run(
                "pip install --no-cache-dir --break-system-packages idna 2>&1 | tail -3",
                { timeout: 240 },
            );
            if (result.exitCode !== 0) {
                throw new Error(`pip exited ${result.exitCode}: ${result.stdout}`);
            }
            return result.stdout.trim().slice(-160);
        });
    }


    await step("A/B: guest TLS tax (preamble vs SNI)", async () => {
        const url = "https://pypi.org/pypi/six/json";

        const wget = await box.commands.run(
            `wget -q -O/dev/null ${url}; ` +
                `for i in 1 2 3; do ` +
                `{ time wget -q -O/dev/null ${url} ; } 2>&1 | grep real; done`,
            { timeout: 120 },
        );

        const python = await box.code.run(
            "import urllib.request, time\n" +
            `u = '${url}'\n` +
            "urllib.request.urlopen(u, timeout=60).read()\n" +
            "for _ in range(3):\n" +
            "    t = time.time()\n" +
            "    urllib.request.urlopen(u, timeout=60).read()\n" +
            "    print(int((time.time() - t) * 1000))\n",
            { timeout: 180 },
        );

        const median = (text) => {
            const values = text
                .split("\n")
                .map((line) => Number(line.trim()))
                .filter((value) => Number.isFinite(value) && value > 0)
                .sort((a, b) => a - b);
            return values.length === 0 ? null : values[Math.floor(values.length / 2)];
        };

        const sni = median(python.text ?? "");

        return `sni(python)=${sni}ms preamble(wget) raw=${JSON.stringify(
            wget.stdout.trim(),
        )}`;
    });


    await step("non-CORS host: refusal, teardown, and a working control", async () => {
        const refused = "https://dl-cdn.alpinelinux.org/alpine/";
        const working = "https://pypi.org/simple/";
        const count = "ps | grep -c '[s]sl_client'";

        const before = await box.commands.run(`echo n=$(${count})`, { timeout: 20 });

        const directAt = performance.now();
        const direct = await box.commands.run(
            `wget -T 3 -q -O/dev/null ${refused} 2>&1; echo EXITED rc=$?`,
            { timeout: 30 },
        );
        const directSeconds = (performance.now() - directAt) / 1000;

        const pipe = async (url) => {
            const startedAt = performance.now();
            const result = await box.commands.run(
                `timeout 20 sh -c 'wget -T 3 -O- ${url} 2>&1 | head -c 200' >/dev/null 2>&1; echo rc=$?`,
                { timeout: 40 },
            );
            return {
                seconds: (performance.now() - startedAt) / 1000,
                rc: (result.stdout.match(/rc=(\d+)/) ?? [, "?"])[1],
            };
        };

        const refusedPipe = await pipe(refused);
        const workingPipe = await pipe(working);

        const after = await box.commands.run(`echo n=$(${count})`, { timeout: 20 });
        const leaked =
            Number((after.stdout.match(/n=(\d+)/) ?? [, "0"])[1]) -
            Number((before.stdout.match(/n=(\d+)/) ?? [, "0"])[1]);

        const alive = await box.commands.run("echo still-alive", { timeout: 20 });

        return (
            `direct=${directSeconds.toFixed(2)}s exited=${direct.stdout.includes("EXITED")} | ` +
            `piped(refused)=${refusedPipe.seconds.toFixed(2)}s rc=${refusedPipe.rc} | ` +
            `piped(working)=${workingPipe.seconds.toFixed(2)}s rc=${workingPipe.rc} | ` +
            `ssl_client leaked=${leaked} | next=${JSON.stringify(alive.stdout.trim())}`
        );
    });

    await step("the installed package imports", async () => {
        const result = await box.code.run("import six; print(six.__version__)", {
            timeout: 60,
        });
        if (!result.success) {
            throw new Error(`import failed: ${result.error}`);
        }
        return result.text.trim();
    });

    await box.close();
    await report({ failed: false, steps, crossOriginIsolated });
} catch (thrown) {
    await report({
        failed: true,
        error: thrown?.stack ?? String(thrown),
        steps,
        crossOriginIsolated,
    });
}
