<h1 align="center"> <code>Vpod</code> TypeScript SDK </h1>

<div align="center">
  <p><strong>A lightweight, portable sandbox that gives an untrusted process an instant Linux environment.</strong></p>
  <a href="https://github.com/capsulerun/vpod"><img src="https://img.shields.io/badge/GitHub-Repository-black?logo=github" alt="GitHub"></a>

[Documentation](https://docs.vpod.sh/quickstart) • [Issues](https://github.com/capsulerun/vpod/issues/new)
</div>

<br>

It uses a RISC‑V architecture and runs entirely inside WebAssembly, which means the
same sandbox runs in a browser tab under the browser's own wasm engine.

- **Fast startup** : under a second, and about 0.6s on a repeat visit.
- **Portable** : a browser tab or Node, no server and no native dependency.
- **Isolated** : all execution state stays inside the WASM sandbox.

## Installation

```bash
npm install vpod
```

## Usage

Everything is async. Loading the engine and fetching a snapshot are genuinely
asynchronous, and exposing execution as async is what lets it run in a Worker.

### Persistent session (Recommended)

All calls share the same running sandbox. `await using` cleans it up when the
scope ends:

```ts
import { Sandbox } from "vpod";

await using sandbox = await Sandbox.create();

await sandbox.commands.run("export FOO=bar");
await sandbox.commands.run("touch /tmp/data.csv");

const result = await sandbox.commands.run("echo $FOO");
console.log(result.stdout); // bar
```

If your toolchain does not support `await using`, call `close()` yourself:

```ts
const sandbox = await Sandbox.create();
try {
    const result = await sandbox.commands.run("echo hello");
    console.log(result.stdout); // hello
} finally {
    await sandbox.close();
}
```

### Python REPL

Run Python with persistent state across calls. Variables and imports live for the
lifetime of the session.

```ts
import { Sandbox } from "vpod";

await using sandbox = await Sandbox.create();

await sandbox.code.run("import json");
await sandbox.code.run("data = [1, 2, 3]");

const result = await sandbox.code.run("print(sum(data))");
console.log(result.text); // 6
```

### Choosing a snapshot

```ts
await using sandbox = await Sandbox.create({ snapshot: "vsnap-data" });

await sandbox.code.run("import pandas as pd");
await sandbox.code.run("print('Pandas is ready!')");
```

### Suspend & Resume

Pause a sandbox and resume it later. Only dirty memory pages are saved, so a
delta is a couple of megabytes rather than the size of the snapshot.

**The delta is bytes, not a location.** Put it wherever you keep state, which for
a hosted app is usually your own backend:

```ts
const sandbox = await Sandbox.create();
await sandbox.commands.run("uv pip install --system requests");

const delta = await sandbox.suspend(); // Uint8Array, ~2 MiB
await sandbox.close();

// Later, possibly in another tab or another process:
const resumed = await Sandbox.resume({
    id: "my-instance",
    snapshotId: sandbox.snapshotId,
    delta,
});
await resumed.code.run("import requests; print(requests.__version__)");
```

In a browser you can let the SDK keep it in origin-private storage instead:

```ts
const instanceId = await sandbox.suspendToOpfs();
const resumed = await Sandbox.resume(instanceId);
```

| Method | Description |
|:---|:---|
| `sandbox.suspend()` | Suspend and return the delta bytes |
| `sandbox.suspendToOpfs()` | Suspend into browser storage, returns an instance id |
| `Sandbox.resume(idOrInstance)` | Resume from an id or from delta bytes |
| `Sandbox.listInstances()` | List instances held in browser storage |
| `Sandbox.destroy(id)` | Delete a stored instance |

### Bringing your own snapshot

Skip the registry and hand over the bytes. Keep the RAM size in the name, because
the emulator reads it from there:

```ts
await using sandbox = await Sandbox.create({
    snapshotBytes: myBytes,
    snapshotName: "vsnap-base-256mb.snap",
});
```

## Results

`commands.run` resolves to a `CommandResult`:

| Field | Description |
|:---|:---|
| `stdout` / `stderr` | captured output, with terminal carriage returns removed |
| `exitCode` | the guest's exit code, `124` on timeout |
| `success` | `exitCode === 0` |

`code.run` resolves to a `CodeExecution`:

| Field | Description |
|:---|:---|
| `text` | the REPL transcript, trimmed |
| `logs` | one entry per output line |
| `error` | the failing line, or `null` |
| `success` | `error === null` |

Both take `{ timeout }` in seconds, defaulting to 120.

## Snapshots

The first `Sandbox.create()` downloads a snapshot and caches it in origin-private
storage. Later visits read it from there and skip the network.

Snapshots are cached **compressed**, which costs a quarter of the storage and
reads back roughly five times faster than the decompressed form, because the
guest decodes it more cheaply than the browser can move the extra bytes.

```ts
import { snapshots } from "vpod";

for (const entry of await snapshots.catalog()) {
    console.log(entry.name, entry.tag);
}
```

### Available snapshots

| Name | Tag | Description | Memory Limit (RAM) |
|:---|:---|:---|:---|
| `alpine` | 3.23.0 | Minimal Alpine Linux snapshot. | 256 MB |
| `vsnap-base` | 1.0.0 | Alpine-based general-purpose snapshot with Python. | 256 MB |
| `vsnap-base-512mb` | 1.0.0 | Same as `vsnap-base` with more memory headroom. | 512 MB |
| `vsnap-data` | 1.0.0 | Alpine-based snapshot with `numpy`, `pandas`, and `scipy`. | 512 MB |

## Browsers

Chrome, Firefox and Safari on desktop, all verified running the same snapshot.
Mobile Safari is untested.

Two hosting requirements:

- **Serve the `.wasm` with immutable cache headers.** Browser wasm code caching
  only happens through `compileStreaming` from a cacheable URL, and it halves
  startup on a repeat visit. Give the assets a URL that changes when you rebuild,
  or a browser will keep the old bundle.
- **Serve over https, or from localhost.** Snapshot digests use `crypto.subtle`,
  which needs a secure context.

Cross-origin isolation (COOP/COEP) is **not** required.

## Node

The same package runs under Node with no browser. Node has no `Worker` global, so
supply the in-process transport:

```ts
import { Sandbox } from "vpod";
import { createInlineTransport } from "vpod/inline";

const sandbox = await Sandbox.create({ transport: await createInlineTransport() });
```

Node has no origin-private storage either, so snapshot caching and
`suspendToOpfs` are unavailable there; `suspend()` still returns the bytes.


## Documentation

Visit the [Vpod documentation](https://docs.vpod.sh/quickstart) for the full guide
and API reference. To report issues or contribute, head to the
[main GitHub repository](https://github.com/capsulerun/vpod).
