<h1 align="center"> <code>Vpod</code> TypeScript SDK </h1>

<div align="center">
  <p><strong>A lightweight, portable sandbox that gives an untrusted process an instant Linux environment.</strong></p>
  <a href="https://github.com/capsulerun/vpod"><img src="https://img.shields.io/badge/GitHub-Repository-black?logo=github" alt="GitHub"></a>
  <a href="https://github.com/capsulerun/vpod/actions/workflows/ci.yml" target="_blank">
    <img src="https://img.shields.io/github/actions/workflow/status/capsulerun/vpod/ci.yml?branch=main&label=CI&logo=github" alt="CI">
  </a>

[Documentation](https://docs.vpod.sh/quickstart) • [Issues](https://github.com/capsulerun/vpod/issues/new)
</div>

<br>

It uses a RISC‑V architecture and runs entirely inside WebAssembly, so the same sandbox runs in a browser tab and in Node.

- **Fast startup** : under a second, and about 0.6s on a repeat visit.
- **Portable** : a browser tab or Node, no server and no native dependency.
- **Isolated** : all execution state stays inside the WASM sandbox.

## Installation

```bash
npm install @capsule-run/vpod
```

## Usage

Everything is async, because loading the engine and fetching a snapshot are.

### Persistent session (Recommended)

All calls share the same running sandbox. `await using` cleans it up when the scope ends:

```ts
import { Sandbox } from "@capsule-run/vpod";

await using sandbox = await Sandbox.create();

await sandbox.commands.run("export FOO=bar");
await sandbox.commands.run("touch /tmp/data.csv");

const result = await sandbox.commands.run("echo $FOO");
console.log(result.stdout); // bar
```

If your toolchain does not support `await using`, call `await sandbox.close()` yourself.

### Python REPL

Run Python with persistent state across calls. Variables and imports live for the lifetime of the session.

```ts
import { Sandbox } from "@capsule-run/vpod";

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

**The delta is bytes, not a location.** Put it wherever you keep state, which for a hosted app is usually your own backend:

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

## Snapshots

The first `Sandbox.create()` downloads a snapshot and caches it locally, in origin-private storage in a browser and on disk in Node. Later runs use the cache.

```ts
import { snapshots } from "@capsule-run/vpod";

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

### Managing the cache

A cached snapshot is a few tens of megabytes, so it is worth being able to show
people what is stored and let them get it back.

```ts
for (const entry of await snapshots.cached()) {
    console.log(entry.id, entry.byteLength);
}

const reclaimed = await snapshots.clear();
```

`clear()` returns the number of bytes it freed and keeps suspended sandboxes,
which are stored alongside the snapshots. Pass `{ instances: true }` to drop
those as well. The next `Sandbox.create()` downloads again.

On Node the cache directory is shared with the other vpod tools, so `clear()`
removes only what it downloaded itself and leaves anything it cannot fetch
again. `cached()` still lists everything that is there.

In a browser this is site data rather than the HTTP cache, so clearing cached
images and files in the browser's own settings will not touch it. A browser is
also free to evict it when the disk fills up, which costs a re-download and
nothing else. `snapshots.SnapshotStore.persist()` asks it not to. Firefox asks
the user, so call it from something they clicked rather than on startup.

## Documentation

Visit the [Vpod documentation](https://docs.vpod.sh/quickstart) for the full
guide and API reference. Implementation notes and measurements live in
[`docs/browser-phases/`](../../docs/browser-phases/00-overview.md). To report
issues or contribute, head to the
[main GitHub repository](https://github.com/capsulerun/vpod).
