from pathlib import Path

from wasmtime import Config, Engine, Linker, Store
import wasmtime.loader

_BUNDLED_WASM = Path(__file__).parent / "vpod_wasi_lib.wasm"


def locate_wasm() -> Path:
    if _BUNDLED_WASM.exists():
        return _BUNDLED_WASM

    raise FileNotFoundError(
        f"WASM library not found at {_BUNDLED_WASM}.\n"
    )


def load_component(wasm_path: Path):
    config = Config()
    config.wasm_component_model = True

    engine = Engine(config)
    store = Store(engine)

    component = wasmtime.loader.Component.from_file(engine, str(wasm_path))
    linker = Linker(engine)
    linker.define_wasi()

    instance = linker.instantiate(store, component)
    exports = instance.exports(store)

    return store, exports
