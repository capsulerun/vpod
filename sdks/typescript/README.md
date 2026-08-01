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
npm install vpod
```

## Usage

Everything is async, because loading the engine and fetching a snapshot are.

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

If your toolchain does not support `await using`, call `await sandbox.close()` yourself.

### Python REPL

Run Python with persistent state across calls. Variables and imports live for the lifetime of the session.

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

Chrome, Firefox and Safari on desktop. Mobile Safari is untested.

Serve the page over https (or localhost), give the `.wasm` a URL that changes when you rebuild, and add these two headers so the guest can have a network:

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

Without them the sandbox still works, offline. `networkAvailability()` says
which you got, and `network: false` turns it off deliberately.

### What the guest can reach

Its requests are **your page's requests**, made with `fetch`, so:

- **Your CORS policy is the boundary.** `pypi.org`, `files.pythonhosted.org`
  and `registry.npmjs.org` allow it, so `pip`, `uv` and `npm` work. The Alpine
  mirrors do not, so `apk` cannot. A refused host reaches the guest as
  `502 Bad Gateway` with the reason in the body.
- **HTTP and HTTPS only**, on ports 80 and 443. No raw TCP, no other port.
- **Some request headers are dropped**, because the browser writes them itself:
  `Host`, `Connection`, `Content-Length`, `Transfer-Encoding`, `Cookie`,
  `Origin`, `Referer`, `Accept-Encoding` and anything `Sec-*` or `Proxy-*`.
- **Certificate policy is the browser's**, and responses are buffered rather
  than streamed.

## Node

Node gets **real sockets**, so none of the above applies. There is no CORS and
no COOP/COEP: the guest simply has a network.

```ts
import { Sandbox } from "@capsule-run/vpod/node";

await using sandbox = await Sandbox.create();

await sandbox.commands.run("apk add jq");        // unreachable from a browser
await sandbox.commands.run("uv pip install six");
```

Snapshots cache in the same directory the Python SDK uses, so both share one
copy. `VPOD_CACHE_DIR` overrides it.

The guest runs on a worker thread, so your event loop keeps turning while a
command does. **Node 20 or newer; 24 and up is meaningfully faster**, since the
guest goes as fast as the V8 your Node ships with.

## Documentation

Visit the [Vpod documentation](https://docs.vpod.sh/quickstart) for the full
guide and API reference. Implementation notes and measurements live in
[`docs/browser-phases/`](../../docs/browser-phases/00-overview.md). To report
issues or contribute, head to the
[main GitHub repository](https://github.com/capsulerun/vpod).
