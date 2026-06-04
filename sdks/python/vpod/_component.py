from pathlib import Path

from wasmtime import Engine, Store, WasiConfig
from wasmtime.component import Component, Linker

_BUNDLED_WASM = Path(__file__).parent / "vpod_wasi_lib.wasm"
_REPO_WASM = Path(__file__).parents[4] / "target" / "wasm32-wasip2" / "release" / "vpod_wasi_lib.wasm"


def locate_wasm() -> Path:
    for candidate in (_BUNDLED_WASM, _REPO_WASM):
        if candidate.exists():
            return candidate

    raise FileNotFoundError(
        f"WASM library not found. Either:\n"
        f"  - Run: cargo build -p wasi-component --lib --release --target wasm32-wasip2\n"
        f"  - Or copy vpod_wasi_lib.wasm to {_BUNDLED_WASM}"
    )


def load_component(wasm_path: Path, snapshot_path: Path = None):
    from . import snapshots as _snapshots
    snap_dir = str(_snapshots.cache_dir()) if snapshot_path is None else str(snapshot_path.parent)

    engine = Engine()
    store = Store(engine)

    wasi = WasiConfig()
    wasi.inherit_stdout()
    wasi.inherit_stderr()
    wasi.inherit_stdin()
    wasi.preopen_dir(snap_dir, snap_dir)
    store.set_wasi(wasi)

    component = Component.from_file(engine, str(wasm_path))
    linker = Linker(engine)
    linker.add_wasip2()

    instance = linker.instantiate(store, component)

    iface_index = instance.get_export_index(store, "vpod:sandbox/executor@0.1.0")
    if iface_index is None:
        raise RuntimeError("WASM component does not export 'vpod:sandbox/executor@0.1.0'")

    def get_export(name: str):
        idx = instance.get_export_index(store, name, iface_index)
        if idx is None:
            raise RuntimeError(f"WASM export '{name}' not found in executor interface")
        func = instance.get_func(store, idx)
        if func is None:
            raise RuntimeError(f"WASM export '{name}' is not a function")
        return lambda *args: func(store, *args)

    exports = {name: get_export(name) for name in ("execute", "session-start", "session-exec", "session-close")}

    return store, exports
