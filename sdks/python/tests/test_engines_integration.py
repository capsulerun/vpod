"""A snapshot with its own engine, end to end, against a local catalogue.
"""

import gzip
import hashlib
import http.server
import json
import os
import subprocess
import sys
import textwrap
import threading
from functools import partial
from pathlib import Path

import pytest

from vpod import snapshots
from vpod._component import locate_wasm
from vpod._engine_interface import fingerprint

pytestmark = pytest.mark.integration

SDK_ROOT = Path(__file__).parents[1]


@pytest.fixture
def catalogue(tmp_path):
    source = snapshots.pull()

    served = tmp_path / "served"
    served.mkdir()
    (served / "snapshot.snap").symlink_to(source)

    engine_bytes = gzip.compress(locate_wasm().read_bytes())
    (served / "engine.wasm.gz").write_bytes(engine_bytes)
    engine_sha256 = hashlib.sha256(engine_bytes).hexdigest()

    handler = partial(http.server.SimpleHTTPRequestHandler, directory=str(served))
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base_url = f"http://127.0.0.1:{server.server_address[1]}"

    ram = source.stem.rsplit("-", 1)[-1]
    snapshot_id = f"vsnap-imagetest-{ram}"
    (served / "snapshots.json").write_text(json.dumps({
        "version": "1",
        "snapshots": [{
            "id": snapshot_id,
            "name": "image-test",
            "tag": "latest",
            "memory_label": ram,
            "description": "per-image engine integration test",
            "url": f"{base_url}/snapshot.snap",
            "sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
            "size": source.stat().st_size,
            "engines": [{
                "vpod_version": "0.8.3",
                "interface": fingerprint(locate_wasm().read_bytes()),
                "url": f"{base_url}/engine.wasm.gz",
                "sha256": engine_sha256,
                "size": len(engine_bytes),
            }],
        }],
    }))

    yield {"registry": f"{base_url}/snapshots.json", "engine_sha256": engine_sha256}
    server.shutdown()


def run_isolated(script: str, home: Path, registry: str) -> str:
    environment = dict(os.environ, HOME=str(home), VPOD_REGISTRY=registry)
    environment.pop("VPOD_SNAPSHOT", None)
    completed = subprocess.run(
        [sys.executable, "-c", textwrap.dedent(script)],
        cwd=SDK_ROOT, env=environment, capture_output=True, text=True, timeout=600,
    )
    assert completed.returncode == 0, completed.stdout + completed.stderr
    return completed.stdout


def test_second_sandbox_starts_on_the_snapshots_own_engine(catalogue, tmp_path):
    home = tmp_path / "home"
    home.mkdir()

    output = run_isolated(f"""
        import time
        from vpod import Sandbox, engines

        first = Sandbox.create("image-test")
        print("first", first.tier, first.commands.run("echo one").stdout)
        first.close()

        deadline = time.monotonic() + 300
        while not list(engines.engines_dir().glob("{catalogue['engine_sha256']}.*.cwasm")):
            assert time.monotonic() < deadline, "the engine was never compiled"
            time.sleep(1)

        second = Sandbox.create("image-test")
        second.commands.run("export LEFT_BY=image")
        print("second", second.tier, second.commands.run("echo two").stdout)

        instance_id = second.suspend()
        resumed = Sandbox.resume(instance_id)
        print("resumed", resumed.tier, resumed.commands.run("echo $LEFT_BY").stdout)
        resumed.close()

        bundled = Sandbox.create("image-test", engine="default")
        print("default", bundled.tier, bundled.commands.run("echo three").stdout)
        bundled.close()
    """, home, catalogue["registry"])

    lines = dict(line.split(" ", 1) for line in output.strip().splitlines())
    assert lines["first"].split()[0] in ("base", "aot")
    assert lines["first"].endswith("one")
    assert lines["second"] == "image two"
    assert lines["resumed"] == "image image"
    assert lines["default"].split()[0] in ("base", "aot")
    assert lines["default"].endswith("three")
