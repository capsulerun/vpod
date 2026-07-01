<h1 align="center"> <code>Vpod</code> Python SDK </h1>

<div align="center">
  <p><strong>A lightweight, portable sandbox that gives an untrusted process an instant Linux environment.</strong></p>
  <a href="https://github.com/capsulerun/vpod"><img src="https://img.shields.io/badge/GitHub-Repository-black?logo=github" alt="GitHub"></a>
  <a href="https://github.com/capsulerun/vpod/actions/workflows/ci.yml" target="_blank">
    <img src="https://img.shields.io/github/actions/workflow/status/capsulerun/vpod/ci.yml?branch=main&label=CI&logo=github" alt="CI">
  </a>
</div>

<br>

It uses a RISC‑V architecture and runs entirely inside WebAssembly.

- **Fast startup** : Boot in under a second.
- **Portable** : Runs anywhere without any setup required.
- **Isolated** : All execution state stays inside the WASM sandbox.

## Installation

```bash
pip install vpod
```

## Usage

### Persistent session (Recommended)

All calls share the same running VM. Using a context manager (`with`) automatically cleans up resources when done:

```python
from vpod import Sandbox

with Sandbox.create() as sbx:
    sbx.commands.run("export FOO=bar")
    sbx.commands.run("touch /tmp/data.csv")

    result = sbx.commands.run("echo $FOO")
    print(result.stdout)  # bar
```

### Python REPL

Run Python code with persistent state across calls. Variables and imports persist for the lifetime of the session.

```python
from vpod import Sandbox

with Sandbox.create() as sbx:
    sbx.code.run("import requests")
    sbx.code.run("data = [1, 2, 3]")
    result = sbx.code.run("print(sum(data))")
    print(result.text)  # 6
```

### Advanced Configuration

You can mount local directories into the sandbox and specify which snapshot to use.

```python
from vpod import Sandbox

# Mount a local workspace and use a snapshot with pre-installed data science tools
mounts = {"workspace": "/workspace:rw"}

with Sandbox.create(snapshot="vsnap-data", mounts=mounts) as sbx:
    sbx.code.run("import pandas as pd")
    sbx.code.run("print('Pandas is ready!')")
```

### Shell commands (stateless)

If you just need a quick one-off execution without preserving state:

```python
from vpod import Sandbox

sbx = Sandbox.create()
result = sbx.commands.run("echo hello")
print(result.stdout)    # hello
sbx.close()             # Clean up the sandbox process
```

## Snapshots

The first call to `Sandbox.create()` downloads the VM snapshot and caches it locally. Subsequent calls use the cache instantly.

To pre-download (e.g. in a Dockerfile or CI setup):

```python
from vpod import snapshots

for s in snapshots.fetch_registry():
    print(s["name"], s["tag"])

snapshots.pull("alpine:latest")
```

### Available Snapshots

| Name | Tag | Description | Memory Limit (RAM) |
|:---|:---|:---|:---|
| `alpine` | 3.23.0 | Minimal Alpine Linux snapshot. | 256 MB |
| `vsnap-base` | 0.1.0 | Alpine-based general-purpose snapshot with Python. | 256 MB |
| `vsnap-data` | 0.1.0 | Alpine-based snapshot with `numpy`, `pandas`, and `scipy`. | 512 MB |

## How it works

A `vpod` runs a RISC‑V virtual machine compiled to WebAssembly. The core implements the **RV64GC** specification:

- **G (General-purpose)**: I/M/A/F/D extensions for integer, multiply/divide, atomics, and floating-point.
- **C (Compressed)**: 30% smaller code size, improving memory efficiency.

The WASM component communicates with the host through WASI 0.2, providing controlled access to networking and I/O while keeping all execution state isolated inside the sandbox.

## Limitations

- **Emulation overhead**: No hardware acceleration in WASM. CPU-intensive workloads run slower than native.
- **No GPU access**: CUDA, Metal, and hardware ML accelerators are not yet available.

For full documentation and to report issues, visit the [main GitHub repository](https://github.com/capsulerun/vpod).
