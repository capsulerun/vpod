import gzip
import json
from pathlib import Path

import lz4.frame
import pytest

import vpod
from vpod import engines, snapshots
from vpod.snapshots import PulledSnapshot

INTERFACE = "v1:" + "a" * 64
OTHER_INTERFACE = "v1:" + "b" * 64
REGISTRY = "https://api.vpod.sh/v1/snapshots.json"
WASM = b"\0asm\x0d\x00\x01\x00" + b"component body"


def engine(sha: str, version: str, interface: str = INTERFACE, url: str | None = "https://blob/e") -> dict:
    listed = {"sha256": sha, "vpod_version": version, "interface": interface}
    if url is not None:
        listed["url"] = url
    return listed


@pytest.fixture
def engine_cache(tmp_path, monkeypatch):
    snapshot_dir = tmp_path / "vpod" / "snapshots"
    snapshot_dir.mkdir(parents=True)
    monkeypatch.setattr(snapshots, "cache_dir", lambda: snapshot_dir)
    monkeypatch.setattr(engines, "referenced_by_instances", lambda: (set(), True))
    directory = tmp_path / "vpod" / "engines"
    directory.mkdir()
    return directory


def own_engine(directory: Path, sha: str, snapshot_id: str, origin: str = REGISTRY) -> None:
    (directory / f"{sha}.download").write_bytes(WASM)
    (directory / f"{sha}.45.0.0.cwasm").write_bytes(b"compiled")
    (directory / f"{sha}.meta").write_text(json.dumps({"snapshot": snapshot_id}))
    (directory / f"{sha}.origin").write_text(origin)


class TestSelect:
    def test_takes_the_newest_engine_with_a_matching_interface(self):
        entry = {"engines": [engine("old", "0.8.0"), engine("new", "0.8.10"), engine("mid", "0.8.9")]}
        assert engines.select(entry, INTERFACE)["sha256"] == "new"

    def test_skips_engines_this_sdk_cannot_run(self):
        entry = {"engines": [engine("fits", "0.8.0"), engine("newer", "0.9.0", OTHER_INTERFACE)]}
        assert engines.select(entry, INTERFACE)["sha256"] == "fits"

    def test_prefers_a_release_over_its_own_release_candidate(self):
        entry = {"engines": [engine("candidate", "0.9.0rc2"), engine("release", "0.9.0")]}
        assert engines.select(entry, INTERFACE)["sha256"] == "release"

    def test_nothing_when_no_engine_fits(self):
        assert engines.select({"engines": [engine("x", "0.8.0", OTHER_INTERFACE)]}, INTERFACE) is None

    def test_nothing_for_a_snapshot_without_engines(self):
        assert engines.select({"id": "vsnap-base-256mb"}, INTERFACE) is None

    def test_nothing_when_the_bundled_engine_could_not_be_fingerprinted(self):
        assert engines.select({"engines": [engine("x", "0.8.0")]}, None) is None

    def test_ignores_an_entry_without_a_url(self):
        assert engines.select({"engines": [engine("x", "0.8.0", url=None)]}, INTERFACE) is None


class TestAnnounce:
    def test_says_when_an_older_engine_is_used(self, monkeypatch, capsys):
        monkeypatch.setattr(engines, "_sdk_version", lambda: "0.8.3")
        chosen = engine("older-announce", "0.8.0")
        engines.announce({"id": "vsnap-x", "engines": [chosen]}, chosen)
        assert "built with vpod 0.8.0" in capsys.readouterr().err

    def test_says_when_no_engine_fits(self, monkeypatch, capsys):
        monkeypatch.setattr(engines, "_sdk_version", lambda: "0.9.0")
        engines.announce({"id": "vsnap-unfit", "engines": [engine("x", "0.8.0")]}, None)
        assert "Rebuild" in capsys.readouterr().err

    def test_is_quiet_for_a_current_engine(self, monkeypatch, capsys):
        monkeypatch.setattr(engines, "_sdk_version", lambda: "0.8.3")
        chosen = engine("current", "0.8.3")
        engines.announce({"id": "vsnap-y", "engines": [chosen]}, chosen)
        assert capsys.readouterr().err == ""


class TestPrune:
    def test_deletes_an_engine_the_catalogue_no_longer_lists(self, engine_cache):
        own_engine(engine_cache, "gone", "vsnap-x")
        engines.prune([{"id": "vsnap-x", "engines": []}], REGISTRY)
        assert list(engine_cache.iterdir()) == []

    def test_keeps_a_listed_engine(self, engine_cache):
        own_engine(engine_cache, "kept", "vsnap-x")
        engines.prune([{"id": "vsnap-x", "engines": [engine("kept", "0.8.0")]}], REGISTRY)
        assert (engine_cache / "kept.download").exists()

    def test_keeps_the_old_engine_until_its_replacement_is_downloaded(self, engine_cache):
        own_engine(engine_cache, "old", "vsnap-x")
        registry = [{"id": "vsnap-x", "engines": [engine("new", "0.8.4")]}]

        engines.prune(registry, REGISTRY)
        assert (engine_cache / "old.download").exists()

        (engine_cache / "new.download").write_bytes(WASM)
        engines.prune(registry, REGISTRY)
        assert not (engine_cache / "old.download").exists()

    def test_leaves_engines_another_registry_listed(self, engine_cache):
        own_engine(engine_cache, "elsewhere", "vsnap-x", origin="https://other/snapshots.json")
        engines.prune([], REGISTRY)
        assert (engine_cache / "elsewhere.download").exists()

    def test_leaves_files_another_sdk_owns(self, engine_cache):
        (engine_cache / "typescript.download").write_bytes(WASM)
        (engine_cache / "typescript.sha256").write_text("typescript")
        engines.prune([], REGISTRY)
        assert (engine_cache / "typescript.download").exists()

    def test_keeps_an_engine_a_suspended_instance_needs(self, engine_cache, monkeypatch):
        own_engine(engine_cache, "suspended", "vsnap-x")
        monkeypatch.setattr(engines, "referenced_by_instances", lambda: ({"suspended"}, True))
        engines.prune([], REGISTRY)
        assert (engine_cache / "suspended.download").exists()

    def test_deletes_nothing_when_an_instance_could_not_be_read(self, engine_cache, monkeypatch):
        own_engine(engine_cache, "unsure", "vsnap-x")
        monkeypatch.setattr(engines, "referenced_by_instances", lambda: (set(), False))
        engines.prune([], REGISTRY)
        assert (engine_cache / "unsure.download").exists()


class FakeComponent:
    compiled_from: list[bytes] = []

    def __init__(self, engine, component_bytes):
        FakeComponent.compiled_from.append(component_bytes)

    def serialize(self):
        return b"compiled"


class TestPrepare:
    @pytest.fixture
    def downloads(self, engine_cache, monkeypatch):
        served = {}

        def fake_download(url, dest, sha256, registry_url=None, api_key=None, decompress=True):
            assert decompress is False
            dest.write_bytes(served["bytes"])

        monkeypatch.setattr(snapshots, "_download_and_decompress", fake_download)
        monkeypatch.setattr("wasmtime.component.Component", FakeComponent)
        monkeypatch.setattr(engines, "_wasmtime_version", lambda: "45.0.0")
        FakeComponent.compiled_from = []
        return served

    def test_downloads_compiles_and_records_ownership(self, engine_cache, downloads):
        downloads["bytes"] = WASM
        compiled = engines.prepare("vsnap-x", engine("abc", "0.8.3"), REGISTRY, None)

        assert compiled == engine_cache / "abc.45.0.0.cwasm"
        assert compiled.read_bytes() == b"compiled"
        assert json.loads((engine_cache / "abc.meta").read_text())["snapshot"] == "vsnap-x"
        assert (engine_cache / "abc.origin").read_text() == REGISTRY
        assert not (engine_cache / "abc.lock").exists()

    def test_compiles_a_gzip_download_decompressed(self, engine_cache, downloads):
        downloads["bytes"] = gzip.compress(WASM)
        engines.prepare("vsnap-x", engine("gz", "0.8.3"), REGISTRY, None)
        assert FakeComponent.compiled_from == [WASM]

    def test_compiles_an_lz4_download_decompressed(self, engine_cache, downloads):
        downloads["bytes"] = lz4.frame.compress(WASM)
        engines.prepare("vsnap-x", engine("lz", "0.8.3"), REGISTRY, None)
        assert FakeComponent.compiled_from == [WASM]

    def test_refuses_a_download_that_is_not_wasm(self, engine_cache, downloads):
        downloads["bytes"] = b"<html>signed url expired</html>"
        with pytest.raises(ValueError):
            engines.prepare("vsnap-x", engine("html", "0.8.3"), REGISTRY, None)
        assert not (engine_cache / "html.download").exists()
        assert not (engine_cache / "html.lock").exists()

    def test_leaves_the_work_to_a_process_already_preparing_it(self, engine_cache, downloads):
        (engine_cache / "busy.lock").write_text("1234")
        assert engines.prepare("vsnap-x", engine("busy", "0.8.3"), REGISTRY, None) is None
        assert FakeComponent.compiled_from == []

    def test_drops_builds_for_other_wasmtime_versions(self, engine_cache, downloads):
        downloads["bytes"] = WASM
        (engine_cache / "abc.44.0.0.cwasm").write_bytes(b"stale")
        engines.prepare("vsnap-x", engine("abc", "0.8.3"), REGISTRY, None)
        assert not (engine_cache / "abc.44.0.0.cwasm").exists()


class TestSandboxEngineChoice:
    @pytest.fixture
    def with_engine(self, monkeypatch, mock_component):
        chosen = engine("image", "0.8.3")
        entry = {"id": "vsnap-x", "engines": [chosen]}
        monkeypatch.setattr(
            snapshots, "_pull",
            lambda name, registry_url=None, api_key=None, engine_mode="default": PulledSnapshot(
                Path("/fake/snapshot.snap"), entry, REGISTRY, None
            ),
        )
        monkeypatch.setattr(engines, "bundled_interface", lambda: INTERFACE)
        monkeypatch.setattr(engines, "announce", lambda entry, chosen: None)

        calls = {"image": [], "bundled": [], "prepared": []}
        exports = mock_component["exports"]

        def fake_image(cwasm, snap, mounts=None):
            calls["image"].append(cwasm)
            return object(), exports

        def fake_bundled(path, snap=None, mounts=None, upgrade_to_aot=True):
            calls["bundled"].append(upgrade_to_aot)
            return object(), exports

        monkeypatch.setattr("vpod.sandbox.load_image_component", fake_image)
        monkeypatch.setattr("vpod.sandbox.load_component", fake_bundled)
        monkeypatch.setattr(
            engines, "prepare_in_background",
            lambda snapshot_id, listed, registry, key: calls["prepared"].append(listed["sha256"]),
        )
        return calls

    def test_starts_on_a_compiled_image_engine(self, with_engine, monkeypatch, tmp_path):
        compiled = tmp_path / "image.cwasm"
        compiled.write_bytes(b"compiled")
        monkeypatch.setattr(engines, "compiled_path", lambda listed: compiled)

        sandbox = vpod.Sandbox.create("vsnap-x")

        assert sandbox.tier == "image"
        assert with_engine["image"] == [compiled]
        assert with_engine["bundled"] == []

    def test_prepares_it_for_next_time_without_compiling_the_bundled_aot(self, with_engine, monkeypatch):
        monkeypatch.setattr(engines, "compiled_path", lambda listed: None)

        vpod.Sandbox.create("vsnap-x")

        assert with_engine["prepared"] == ["image"]
        assert with_engine["bundled"] == [False]

    def test_default_ignores_the_image_engine(self, with_engine, monkeypatch):
        monkeypatch.setattr(engines, "compiled_path", lambda listed: Path("/never/used.cwasm"))

        vpod.Sandbox.create("vsnap-x", engine="default")

        assert with_engine["image"] == []
        assert with_engine["prepared"] == []
        assert with_engine["bundled"] == [True]

    def test_falls_back_to_the_bundled_engine_when_the_image_engine_will_not_load(
        self, with_engine, monkeypatch, tmp_path
    ):
        compiled = tmp_path / "broken.cwasm"
        compiled.write_bytes(b"not loadable")
        monkeypatch.setattr(engines, "compiled_path", lambda listed: compiled if compiled.exists() else None)

        def refuse(cwasm, snap, mounts=None):
            raise RuntimeError("incompatible cwasm")

        monkeypatch.setattr("vpod.sandbox.load_image_component", refuse)

        sandbox = vpod.Sandbox.create("vsnap-x")

        assert sandbox.tier != "image"
        assert not compiled.exists()
        assert with_engine["prepared"] == ["image"]

    def test_refuses_an_unknown_engine_mode(self):
        with pytest.raises(ValueError):
            vpod.Sandbox.create("vsnap-x", engine="fast")
