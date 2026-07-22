# v0.5.0 — Full-system acceleration

This release is all about speed. The sandbox keeps everything that makes it a vpod (portable WebAssembly, full Linux, two layers of isolation) and gets dramatically faster at the things you actually do in it: CPU-bound Python runs up to ~5× faster, HTTPS fetches went from ~3s to ~0.4s, and a fresh install boots its first sandbox in under a second.


## Ahead-of-time translation

The big one. Pure instruction-by-instruction emulation was the floor holding everything back, and WebAssembly rules out a runtime JIT. So we moved the work to build time: the hottest guest code paths are translated from RISC-V into native code that gets compiled into the engine itself. At runtime the emulator dispatches straight into these translated blocks and falls back to the interpreter whenever the code doesn't match.

Isolation is untouched. Translated code goes through the same MMU and memory checks as interpreted code, and everything still lives inside the WASM sandbox.

> [!NOTE]
> `uv` now comes preinstalled in every snapshot, so Python packages install fast out of the box (`uv pip install --system <package>`). `apk add` is still there for system packages.

## Quality of life

- **Command timeouts**: `commands.run(cmd, timeout=...)` and `code.run(code, timeout=...)` let you bound long-running guest work instead of waiting on the default.
- **New snapshot `vsnap-base-512mb`**: same contents as `vsnap-base` with double the memory headroom, for running web servers, daemons, or larger installs inside the sandbox: `Sandbox.create(snapshot="vsnap-base-512mb")`.
- **Honest speed reporting**: if the bundled AOT module doesn't match your snapshot, the SDK now warns you instead of silently running at interpreter speed, and the snapshot registry cache refreshes automatically when you upgrade the package.

## Upgrading

```bash
pip install --upgrade vpod
```

Existing snapshots and suspended instances keep working. The snapshot format is unchanged, and the SDK re-fetches the registry on upgrade so you always validate against current snapshots.


