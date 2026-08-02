import pytest

from vpod import snapshots
from vpod.snapshots import (
    PUBLIC_REGISTRY_URL,
    _prune_stale_snapshots,
    _resolve_registry_url,
    resolve_snapshot,
)

CHANNEL = "https://registry.vpod.sh/v1/nextjs/snapshots.json"


class TestResolveRegistryUrl:
    def test_defaults_to_the_public_registry(self, monkeypatch):
        monkeypatch.delenv("VPOD_REGISTRY", raising=False)
        assert _resolve_registry_url(None) == PUBLIC_REGISTRY_URL

    def test_takes_an_explicit_url_which_is_how_a_partner_reaches_a_channel(self, monkeypatch):
        monkeypatch.delenv("VPOD_REGISTRY", raising=False)
        assert _resolve_registry_url(CHANNEL) == CHANNEL

    def test_falls_back_to_the_environment(self, monkeypatch):
        monkeypatch.setenv("VPOD_REGISTRY", CHANNEL)
        assert _resolve_registry_url(None) == CHANNEL

    def test_explicit_url_wins_over_the_environment(self, monkeypatch):
        monkeypatch.setenv("VPOD_REGISTRY", CHANNEL)
        assert _resolve_registry_url(PUBLIC_REGISTRY_URL) == PUBLIC_REGISTRY_URL


class TestPruneRespectsOrigin:
    """The GC must only ever judge a file against the registry that served it."""

    @staticmethod
    def _snapshot(cache, snapshot_id, origin):
        (cache / f"{snapshot_id}.snap").write_bytes(b"VPOD")
        (cache / f"{snapshot_id}.meta").write_text("sha")
        if origin is not None:
            (cache / f"{snapshot_id}.origin").write_text(origin)

    @pytest.fixture
    def cache(self, tmp_path, monkeypatch):
        monkeypatch.setattr(snapshots, "cache_dir", lambda: tmp_path)
        monkeypatch.setattr(
            snapshots, "_snapshots_referenced_by_instances", lambda: (set(), True)
        )
        return tmp_path

    def test_public_pull_does_not_delete_channel_snapshots(self, cache):
        self._snapshot(cache, "vsnap-9SdKWt", CHANNEL)

        _prune_stale_snapshots([{"id": "alpine-3.23.0-256mb"}], PUBLIC_REGISTRY_URL)

        assert (cache / "vsnap-9SdKWt.snap").exists()

    def test_channel_pull_does_not_delete_public_snapshots(self, cache):
        self._snapshot(cache, "alpine-3.23.0-256mb", PUBLIC_REGISTRY_URL)

        _prune_stale_snapshots([{"id": "vsnap-9SdKWt"}], CHANNEL)

        assert (cache / "alpine-3.23.0-256mb.snap").exists()

    def test_still_removes_what_its_own_registry_dropped(self, cache):
        self._snapshot(cache, "alpine-3.22.0-256mb", PUBLIC_REGISTRY_URL)

        _prune_stale_snapshots([{"id": "alpine-3.23.0-256mb"}], PUBLIC_REGISTRY_URL)

        assert not (cache / "alpine-3.22.0-256mb.snap").exists()
        assert not (cache / "alpine-3.22.0-256mb.meta").exists()
        assert not (cache / "alpine-3.22.0-256mb.origin").exists()

    def test_leaves_snapshots_with_no_recorded_origin_alone(self, cache):
        self._snapshot(cache, "alpine-3.22.0-256mb", None)

        _prune_stale_snapshots([{"id": "alpine-3.23.0-256mb"}], PUBLIC_REGISTRY_URL)

        assert (cache / "alpine-3.22.0-256mb.snap").exists()


class TestResolveSnapshotError:
    """The failure a partner hits when their VPOD_REGISTRY did not take effect."""

    listed = [{"id": "vsnap-base-256mb", "name": "vsnap-base", "tag": "1.0.0"}]

    def test_names_the_registry_it_searched(self):
        with pytest.raises(ValueError, match=f"not found in {PUBLIC_REGISTRY_URL}"):
            resolve_snapshot(self.listed, "vsnap-nextjs", PUBLIC_REGISTRY_URL)

    def test_still_lists_what_is_available(self):
        with pytest.raises(ValueError, match=r"vsnap-base:1\.0\.0"):
            resolve_snapshot(self.listed, "nope", PUBLIC_REGISTRY_URL)

    def test_omits_the_registry_when_not_given(self):
        with pytest.raises(ValueError, match=r"not found\. Available"):
            resolve_snapshot(self.listed, "nope")

    def test_says_nothing_rather_than_trailing_off(self):
        with pytest.raises(ValueError, match="Available: nothing"):
            resolve_snapshot([], "nope", PUBLIC_REGISTRY_URL)
