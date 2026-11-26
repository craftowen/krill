#!/usr/bin/env python3
"""GitHub workflow supply-chain and trigger contracts that actionlint does not check."""

from pathlib import Path
import re
import sys
import unittest


ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = sorted((ROOT / ".github/workflows").glob("*.yml"))
sys.path.insert(0, str(ROOT / "packaging"))
import ci_evidence  # noqa: E402
import ci_shards  # noqa: E402


class WorkflowTests(unittest.TestCase):
    def test_external_actions_are_pinned_to_a_commit_sha(self):
        # .github/dependabot.yml policy: tags can be moved, commit SHAs cannot.
        self.assertTrue(WORKFLOWS)
        for workflow in WORKFLOWS:
            for number, line in enumerate(workflow.read_text().splitlines(), 1):
                match = re.match(r"\s*(?:-\s*)?uses:\s*(\S+)", line)
                if not match or match[1].startswith("./"):
                    continue
                with self.subTest(workflow=workflow.name, line=number):
                    self.assertRegex(match[1], r"^[\w.-]+/[\w./-]+@[0-9a-f]{40}$")

    def test_local_ci_and_nightly_build_tests_with_the_same_profile(self):
        # Test profile optimization is defined centrally in root Cargo.toml ([profile.test] and
        # [profile.test.package."*"]) so local check.sh, CI, and nightly do not override it with env vars.
        cargo = (ROOT / "Cargo.toml").read_text()
        ci = (ROOT / ".github/workflows/ci-macos.yml").read_text()
        check = (ROOT / "scripts/check.sh").read_text()
        nightly = (ROOT / "packaging/nightly.py").read_text()
        self.assertRegex(cargo, r'(?ms)^\[profile\.test\]\n(?:[^\[]*\n)*?debug\s*=\s*"line-tables-only"')
        self.assertRegex(cargo, r'(?ms)^\[profile\.test\.package\."\*"]\n(?:[^\[]*\n)*?opt-level\s*=\s*2\b')
        for key in ("CARGO_PROFILE_TEST_OPT_LEVEL", "CARGO_PROFILE_TEST_DEBUG"):
            with self.subTest(key=key):
                self.assertNotRegex(ci, r"\b" + key + r":")
                self.assertNotRegex(check, r"(?m)^export .*\b" + key + r"=")
                self.assertNotIn(key + "=", nightly)

    def test_ci_cache_scope_reuse_sharding_and_release_audit_contracts(self):
        ci_shards.verify(ROOT)
        macos = (ROOT / ".github/workflows/ci-macos.yml").read_text()
        windows = (ROOT / ".github/workflows/ci-windows.yml").read_text()
        release = (ROOT / ".github/workflows/release.yml").read_text()

        # ci-macos: <= 5 concurrent macos-15 runners (clippy + 3 shards + packaging),
        # single cache writer shard (`gui`) plus dev-smoke, shard completeness & flaky checks.
        self.assertIn("branches: [dev, main, 'release/**']", macos)
        self.assertIn("smoke & cache warm (macos-15)", macos)
        self.assertIn("name: clippy (macos-15)", macos)
        self.assertIn("name: nextest (macos-15, ${{ matrix.shard }})", macos)
        self.assertIn("shard: [gui, daemon, core]", macos)
        self.assertIn("python3 packaging/ci_shards.py run ${{ matrix.shard }} --timings", macos)
        self.assertIn("python3 packaging/ci_evidence.py flaky", macos)
        self.assertIn("matrix.shard == 'gui'", macos)
        self.assertIn("python3 packaging/ci_cache_prune.py", macos)
        for shard in ci_shards.SHARDS:
            self.assertIn(f"nextest (macos-15, {shard})", ci_evidence.CHECKS["ci-macos.yml"])
        self.assertIn("clippy (macos-15)", ci_evidence.CHECKS["ci-macos.yml"])

        # ci-windows: vendor staging + 3 shards, single test cache writer (`gui`), fast dev release profile.
        self.assertIn("branches: [dev, main, 'release/**']", windows)
        self.assertIn("name: reuse-windows-ci", windows)
        self.assertIn("ci_evidence.py reuse --workflow ci-windows.yml", windows)
        self.assertIn("name: nextest (MSVC, ${{ matrix.shard }})", windows)
        self.assertIn("shard: [gui, daemon, core]", windows)
        self.assertIn("python packaging/ci_shards.py run ${{ matrix.shard }}", windows)
        self.assertIn("matrix.shard == 'gui'", windows)
        self.assertEqual(len(re.findall(r"uses:\s*Swatinem/rust-cache@[0-9a-f]{40}", windows)), 3)
        self.assertEqual(len(re.findall(r"packaging/vendor_cache\.py stage", windows)), 3)
        self.assertIn("CARGO_PROFILE_RELEASE_LTO=off", windows)
        self.assertIn("CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16", windows)
        self.assertIn("python3 packaging/ci_cache_prune.py", windows)
        for shard in ci_shards.SHARDS:
            self.assertIn(f"nextest (MSVC, {shard})", ci_evidence.CHECKS["ci-windows.yml"])

        # release.yml: build-windows keeps canonical Cargo.toml profile, audit-windows runs in parallel
        # with cached +crt-static fast profile, publish-windows & ci_evidence require both.
        build_job = release.split("\n  build-windows:\n", 1)[1].split("\n  audit-windows:\n", 1)[0]
        audit_job = release.split("\n  audit-windows:\n", 1)[1].split("\n  prune-caches:\n", 1)[0]
        publish_job = release.split("\n  publish-windows:\n", 1)[1]
        self.assertNotIn("CARGO_PROFILE_RELEASE_LTO", build_job)
        self.assertNotIn("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", build_job)
        self.assertIn("packaging/vendor_cache.py stage", build_job)
        self.assertIn("CARGO_PROFILE_RELEASE_OPT_LEVEL: '0'", audit_job)
        self.assertIn("CARGO_PROFILE_RELEASE_LTO: 'off'", audit_job)
        self.assertIn("CARGO_PROFILE_RELEASE_CODEGEN_UNITS: '16'", audit_job)
        self.assertIn("RUSTFLAGS: -C target-feature=+crt-static", audit_job)
        self.assertIn("Swatinem/rust-cache@", audit_job)
        self.assertIn("needs: [release, build-windows, audit-windows]", publish_job)
        self.assertIn("needs.build-windows.result == 'success' && needs.audit-windows.result == 'success'", publish_job)
        self.assertEqual(ci_evidence.CHECKS["release.yml"], {"build-windows", "audit-windows"})


if __name__ == "__main__":
    unittest.main()
