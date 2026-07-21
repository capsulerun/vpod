import sys
import time
from pathlib import Path

from wasmtime import Config, Engine

from ._component import _cwasm_cache_path, _compile_and_cache, retire_base_cwasm


def main() -> int:
    args = sys.argv[1:]
    parallel = "--parallel" in args
    fallback = "--fallback" in args
    args = [a for a in args if a not in ("--parallel", "--fallback")]

    if len(args) != 1:
        print("usage: python -m vpod._precompile <wasm_path> [--parallel] [--fallback]", file=sys.stderr)
        return 2

    wasm_path = Path(args[0])
    if not wasm_path.exists():
        return 1

    if fallback:
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if _cwasm_cache_path(wasm_path).exists():
                retire_base_cwasm()
                return 0
            time.sleep(1)

    config = Config()
    config.parallel_compilation = parallel

    _compile_and_cache(Engine(config), wasm_path)

    retire_base_cwasm()
    return 0


if __name__ == "__main__":
    sys.exit(main())
