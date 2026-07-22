"""Check that `uv pip install` packages are importable everywhere.

One sandbox, one install, then the same import exercised through both
execution paths: commands.run (fresh python3 process via the prefork
daemon) and code.run (the long-lived pyrunner session). The pyrunner
path is the one that can go stale — its FileFinder caches predate the
install — so a pass here proves the whole chain, not just the daemon.
"""

import time

from vpod import Sandbox

# name on PyPI -> (module to import, expression proving it actually works)
PACKAGES = {
    "six": ("six", "six.__version__"),
    # double quotes inside: the expression rides in a `python3 -c '...'`
    "requests": ("requests", 'requests.utils.quote("a b")'),
}


def check(label, result, expect=None):
    # commands.run gives stdout/stderr; code.run gives text/error
    out = (getattr(result, "stdout", None) or getattr(result, "text", "")).strip()
    err = (getattr(result, "stderr", None) or getattr(result, "error", None) or "").strip()
    ok = result.success and (expect is None or expect in out)
    print(f"  [{'ok' if ok else 'FAIL'}] {label}: {out or err}")
    return ok


def main():
    failures = 0
    with Sandbox.create() as sandbox:
        # settle daemon + network before anything is timed or asserted
        sandbox.commands.run("python3 -c pass")

        # the published registry snapshot predates uv being baked in
        if not sandbox.commands.run("command -v uv").success:
            print("uv missing from snapshot, apk-installing it...")
            # old snapshot also lacks the vpod CA, so bootstrap over http
            # the same way build-default-snapshot.sh does
            r = sandbox.commands.run(
                "sed -i 's|https:|http:|' /etc/apk/repositories"
                " && apk update && apk add uv",
                timeout=300,
            )
            if not r.success:
                print(f"cannot install uv: {(r.stderr or r.stdout).strip()[-300:]}")
                raise SystemExit(1)

        for pkg, (module, expr) in PACKAGES.items():
            print(f"\n── {pkg} ──")

            # import must fail before the install, or the test proves nothing
            pre = sandbox.commands.run(f"python3 -c 'import {module}'")
            if pre.success:
                print(f"  [skip] {module} already importable before install")
                continue

            t0 = time.monotonic()
            r = sandbox.commands.run(
                f"uv pip install --system --break-system-packages {pkg}",
                timeout=300,
            )
            print(f"  install: {time.monotonic() - t0:.2f}s")
            if not r.success:
                print(f"  [FAIL] install: {(r.stderr or r.stdout).strip()[-300:]}")
                failures += 1
                continue

            failures += 0 if check(
                f"commands.run import {module}",
                sandbox.commands.run(
                    f"python3 -c 'import {module}; print({expr})'"
                ),
            ) else 1

            failures += 0 if check(
                f"code.run import {module}",
                sandbox.code.run(f"import {module}\nprint({expr})"),
            ) else 1

    print(f"\n{'PASS' if failures == 0 else f'{failures} FAILURE(S)'}")
    raise SystemExit(1 if failures else 0)


if __name__ == "__main__":
    main()
