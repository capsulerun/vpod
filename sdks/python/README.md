# vpod Python SDK

A lightweight, portable sandbox that gives an untrusted process an instant Linux environment. It uses the RISC‑V architecture and runs entirely inside WebAssembly.

- **Fast startup** — Boot in under a second.
- **Portable** — Runs anywhere without any setup required.
- **Isolated** — All execution state stays inside the WASM sandbox.

## Installation

```bash
pip install vpod
```

## Usage

### Shell commands (stateless)

Each call gets a fresh VM — no shared state:

```python
from vpod import Sandbox

sbx = Sandbox.create()
result = sbx.commands.run("echo hello")
print(result.stdout)    # hello
print(result.exit_code) # 0
```

### Persistent session

All calls share the same running VM:

```python
from vpod import Sandbox

with Sandbox.create() as sbx:
    sbx.commands.run("export Foo=Bar")
    sbx.commands.run("touch /tmp/data.csv")

    result = sbx.commands.run("echo $Foo")
    print(result.stdout)  # Bar
```

### Python REPL

Run Python code with persistent state across calls:

```python
from vpod import Sandbox

with Sandbox.create() as sbx:
    sbx.code.run("import requests")
    sbx.code.run("data = [1, 2, 3]")
    result = sbx.code.run("print(sum(data))")
    print(result.text)  # 6
```

Variables and imports persist for the lifetime of the session.

### Snapshots

The first call to `Sandbox.create()` downloads the VM snapshot (~50MB) and caches it locally at `~/.local/share/vpod/snapshots/`. Subsequent calls use the cache instantly.

To pre-download (e.g. in a Dockerfile or CI setup):

```python
from vpod import snapshots

for s in snapshots.fetch_registry():
    print(s["name"], s["tag"])

path = snapshots.pull("alpine:latest")
```

## How it works

A vpod runs a RISC‑V virtual machine compiled to WebAssembly. The core implements the **RV64GC** specification:

- **G (General-purpose)**: I/M/A/F/D extensions for integer, multiply/divide, atomics, and floating-point
- **C (Compressed)**: 30% smaller code size, improving memory efficiency

The WASM component communicates with the host through WASI 0.2, providing controlled access to networking and I/O while keeping all execution state isolated inside the sandbox.

## Limitations

- **Emulation overhead** — No hardware acceleration in WASM. CPU-intensive workloads run slower than native.
- **No GPU access** — CUDA, Metal, and hardware ML accelerators are not yet available.
