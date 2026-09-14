import os

from vpod._component import _is_sdk_cwasm_name, _prune_stale_cwasm


class TestCompileCacheOwnership:
    def test_recognises_its_own_caches(self):
        assert _is_sdk_cwasm_name("component-0.8.3-aot-3f2a9c01d4e5b6a7.cwasm")
        assert _is_sdk_cwasm_name("component-0.8.3-base-3f2a9c01d4e5b6a7.cwasm")

    def test_leaves_the_cli_cache_alone(self):
        assert not _is_sdk_cwasm_name("component-0.8.3-c33262116cd6920b.cwasm")

    def test_leaves_anything_else_alone(self):
        assert not _is_sdk_cwasm_name("component-0.8.3-aot-notadigest.cwasm")
        assert not _is_sdk_cwasm_name("component-0.8.3-aot-3f2a9c01d4e5b6a7.1234.tmp")
        assert not _is_sdk_cwasm_name("snapshots")

    def test_pruning_keeps_the_cli_cache_however_old_it_is(self, tmp_path):
        cli_cache = tmp_path / "component-0.8.3-c33262116cd6920b.cwasm"
        sdk_caches = [
            tmp_path / f"component-0.8.{minor}-aot-3f2a9c01d4e5b6a{minor}.cwasm" for minor in range(4)
        ]
        cli_cache.write_bytes(b"cli")
        os.utime(cli_cache, (1, 1))
        for age, cache in enumerate(sdk_caches):
            cache.write_bytes(b"sdk")
            os.utime(cache, (100 + age, 100 + age))

        _prune_stale_cwasm(sdk_caches[-1])

        assert cli_cache.exists()
        assert [cache.exists() for cache in sdk_caches] == [False, False, True, True]
