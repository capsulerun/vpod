from pathlib import Path

from wasmtime import Engine, Store
from wasmtime.component import Component, Linker

_BUNDLED_WASM = Path(__file__).parent / "vpod_wasi_lib.wasm"


def locate_wasm() -> Path:
    if _BUNDLED_WASM.exists():
        return _BUNDLED_WASM

    raise FileNotFoundError(f"WASM library not found at {_BUNDLED_WASM}.\n")


def load_component(wasm_path: Path):
    engine = Engine()
    store = Store(engine)

    component = Component.from_file(engine, str(wasm_path))
    linker = Linker(engine)
    linker.add_wasip2()

    instance = linker.instantiate(store, component)
    exports = instance.exports(store)

    return store, exports
