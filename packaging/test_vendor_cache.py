#!/usr/bin/env python3
"""Cache corruption, source identity and experiment comparability regressions."""

import copy
import json
import os
from pathlib import Path
import tempfile
import unittest

from vendor_cache import CASES, HELPERS, STAGES, cargo_messages, compare, digest_tree, normalized_graph, prepare, stage, verify_stage


class StagingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.project = self.root / "project"
        source = self.project / "vendor/dependency/src"
        source.mkdir(parents=True)
        (source / "lib.rs").write_text("pub fn value() -> u8 { 1 }\n")
        (self.project / "Cargo.toml").write_text('[workspace]\n[patch.crates-io]\ndependency = { path = "vendor/dependency" }\n')
        (self.project / "Cargo.lock").write_text("unchanged\n")

    def run_stage(self, case="producer"):
        return stage(self.project, self.root / "home", self.root / "external", self.root / "evidence", case)

    def test_external_config_preserves_source_and_copies_bytes_and_modes(self):
        before = digest_tree(self.project)
        result = self.run_stage()
        self.assertEqual(before, digest_tree(self.project))
        self.assertEqual(result["vendor_hash"], digest_tree(Path(result["vendor_directory"])))
        self.assertIn(result["vendor_directory"], (self.root / "home/config.toml").read_text())
        self.assertEqual(result, self.run_stage())

    def test_control_does_not_install_a_global_patch(self):
        self.run_stage("control")
        self.assertFalse((self.root / "home/config.toml").exists())

    def test_existing_config_and_source_inside_workspace_are_rejected(self):
        home = self.root / "home"
        home.mkdir()
        (home / "config.toml").write_text("existing configuration\n")
        with self.assertRaisesRegex(ValueError, "existing Cargo"):
            self.run_stage()
        self.assertEqual((home / "config.toml").read_text(), "existing configuration\n")
        with self.assertRaisesRegex(ValueError, "outside"):
            stage(self.project, self.project / "home", self.root / "external", self.root / "evidence", "producer")

    def test_changed_bytes_or_permissions_change_key_even_with_same_mtime(self):
        path = self.project / "vendor/dependency/src/lib.rs"
        first = digest_tree(self.project / "vendor")
        before = path.stat()
        path.write_text(path.read_text().replace("1", "2"))
        os.utime(path, ns=(before.st_atime_ns, before.st_mtime_ns))
        self.assertEqual(before.st_mtime_ns, path.stat().st_mtime_ns)
        second = digest_tree(self.project / "vendor")
        self.assertNotEqual(first, second)
        path.chmod(0o700)
        self.assertNotEqual(second, digest_tree(self.project / "vendor"))

    def test_corrupt_existing_or_restored_source_is_not_used(self):
        record = self.run_stage()
        (Path(record["vendor_directory"]) / "dependency/src/lib.rs").write_text("corrupt")
        with self.assertRaisesRegex(ValueError, "differ"):
            self.run_stage()
        with self.assertRaisesRegex(ValueError, "restored vendor"):
            prepare(self.root / "evidence")

    def test_symlink_input_is_rejected(self):
        (self.project / "vendor/link").symlink_to(self.project / "Cargo.lock")
        with self.assertRaisesRegex(ValueError, "symlink"):
            self.run_stage()

    def test_changed_global_configuration_is_rejected(self):
        self.run_stage()
        (self.root / "home/config.toml").write_text("different config\n")
        with self.assertRaisesRegex(ValueError, "configuration changed"):
            verify_stage(self.root / "evidence")


class GraphTests(unittest.TestCase):
    def test_path_relocation_preserves_graph_but_feature_change_does_not(self):
        a = {"packages": [{"id": "a", "name": "app", "version": "1", "source": None},
                          {"id": "b", "name": "dep", "version": "1", "source": None}],
             "workspace_members": ["a"], "resolve": {"nodes": [
                 {"id": "a", "features": [], "deps": [{"name": "dep", "pkg": "b", "dep_kinds": []}]},
                 {"id": "b", "features": ["native"], "deps": []}]}}
        b = json.loads(json.dumps(a).replace('"b"', '"external"'))
        self.assertEqual(normalized_graph(a), normalized_graph(b))
        b["resolve"]["nodes"][1]["features"] = []
        self.assertNotEqual(normalized_graph(a), normalized_graph(b))


class ComparisonTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.jobs = {"jobs": []}
        for case in CASES:
            folder = self.root / case
            folder.mkdir()
            self.write(case, "metadata.json", dict(case=case, sha="a" * 40, tree="b" * 40,
                run_id="10", attempt="1", rustc="1.98.1", cargo="1.98.1", nextest="same",
                system=["arm64", "14", 3, "7GB"], sdk="14.5", image=["macos14", "same"],
                test_profile=["2", "line-tables-only"], cache=["true", 1, "seed"],
                vendor_hash="v", graph_sha256="g", vendor_cache_hit="true" if case == "consumer" else "false"))
            self.write(case, "vendor.json", {"patches": {"dep": "dep"}})
            self.write(case, "test-list.json", {"test-count": 2, "rust-suites": {"app": {
                "status": "listed", "package-name": "app", "testcases": {
                    "works": {"ignored": False, "filter-match": {"status": "matches"}},
                    "ignored": {"ignored": True, "filter-match": {"status": "mismatch", "reason": "ignored"}}}}}})
            (folder / "source-status.txt").write_text("")
            (folder / "junit.xml").write_text('<testsuites><testsuite name="app"><testcase classname="app" name="works"/></testsuite></testsuites>')
            profile = {"opt_level": "2", "debuginfo": "line-tables-only", "debug_assertions": True,
                       "overflow_checks": True, "test": False}
            artifact = {"package": ["dep", "1", "path"], "target": ["dep", ["lib"]],
                        "features": [], "profile": profile, "fresh": case == "consumer", "executable": None}
            app = copy.deepcopy(artifact)
            app["package"][0] = "krill-app"
            app["target"][0] = "krill_app"
            self.write(case, "compilation.json", {"artifacts": [artifact, app], "clippy_artifacts": [copy.deepcopy(artifact)], "build": "same+dev.id", "helpers": sorted(HELPERS)})
            (folder / "cargo-timing.html").write_text('const UNIT_DATA = [{"start":1,"duration":100}];')
            self.jobs["jobs"].append(dict(name=f"{case} / vendor-cache-{case} (macos-14)", head_sha="a" * 40,
                run_id=10, run_attempt=1, conclusion="success", created_at="2026-10-26T00:00:00Z",
                started_at="2026-10-26T00:00:05Z", completed_at="2026-10-26T00:05:00Z",
                steps=[{"name": s, "conclusion": "success"} for s in STAGES]))

    def write(self, case, name, value):
        (self.root / case / name).write_text(json.dumps(value))

    def change(self, case, name, update):
        path = self.root / case / name
        value = json.loads(path.read_text())
        update(value)
        path.write_text(json.dumps(value))

    def test_complete_matching_comparison_includes_queue_and_post(self):
        rows, report = compare(self.root, self.jobs)
        self.assertEqual(rows[0]["job_seconds"], 295)
        self.assertEqual(rows[0]["gate_seconds"], 300)
        self.assertIn("does not adopt", report)

    def test_broken_json_and_equal_count_wrong_test_ids_are_rejected(self):
        with self.assertRaises(ValueError):
            list(cargo_messages('[krill-proto 0.2.4] cargo:rerun-if-changed=build.rs\n'))
        with self.assertRaises(ValueError):
            list(cargo_messages('{"reason":"compiler-artifact"}\nbroken JSON\n'))
        path = self.root / "consumer/junit.xml"
        path.write_text(path.read_text().replace('name="works"', 'name="different"'))
        with self.assertRaisesRegex(ValueError, "listing"):
            compare(self.root, self.jobs)

    def test_app_build_script_keeps_its_unoptimized_build_override(self):
        for case in CASES:
            def add_build_script(value):
                script = copy.deepcopy(value["artifacts"][1])
                script["target"] = ["build-script-build", ["custom-build"]]
                script["profile"].update(opt_level="0", debuginfo="none", test=False)
                value["artifacts"].append(script)
            self.change(case, "compilation.json", add_build_script)
        rows, _ = compare(self.root, self.jobs)
        self.assertEqual(len(rows), 3)

    def test_corrupt_test_identity_ignored_reason_features_profile_and_build_are_rejected(self):
        edits = [
            ("test-list.json", lambda v: v["rust-suites"]["app"]["testcases"]["ignored"]["filter-match"].update(reason="other")),
            ("compilation.json", lambda v: v["artifacts"][0].update(features=["different"])),
            ("compilation.json", lambda v: v.update(build="different")),
            ("compilation.json", lambda v: v["artifacts"][1]["profile"].update(opt_level="0")),
            ("compilation.json", lambda v: v["artifacts"][1]["profile"].update(debuginfo="none")),
            ("metadata.json", lambda v: v.update(graph_sha256="different")),
            ("metadata.json", lambda v: v.update(sha="c" * 40)),
        ]
        for name, edit in edits:
            with self.subTest(name=name, edit=edit):
                original = (self.root / "consumer" / name).read_text()
                self.change("consumer", name, edit)
                with self.assertRaises(ValueError):
                    compare(self.root, self.jobs)
                (self.root / "consumer" / name).write_text(original)

    def test_cache_miss_dirty_vendor_different_seed_and_missing_helper_are_rejected(self):
        edits = [("metadata.json", lambda v: v.update(vendor_cache_hit="false")),
                 ("compilation.json", lambda v: v["artifacts"][0].update(fresh=False)),
                 ("compilation.json", lambda v: v["clippy_artifacts"][0].update(fresh=False)),
                 ("compilation.json", lambda v: v.update(helpers=["krill"]))]
        for name, edit in edits:
            with self.subTest(edit=edit):
                original = (self.root / "consumer" / name).read_text()
                self.change("consumer", name, edit)
                with self.assertRaises(ValueError):
                    compare(self.root, self.jobs)
                (self.root / "consumer" / name).write_text(original)
        self.change("producer", "metadata.json", lambda v: v.update(cache=["true", 2, "other"]))
        with self.assertRaisesRegex(ValueError, "seed"):
            compare(self.root, self.jobs)

    def test_missing_failed_or_wrong_job_and_stage_are_rejected(self):
        for edit in (lambda v: v["jobs"][1].update(conclusion="failure"),
                     lambda v: v["jobs"][1].update(head_sha="c" * 40),
                     lambda v: v["jobs"][1]["steps"][0].update(conclusion="skipped"),
                     lambda v: v["jobs"].pop()):
            jobs = copy.deepcopy(self.jobs)
            edit(jobs)
            with self.assertRaises(ValueError):
                compare(self.root, jobs)


if __name__ == "__main__":
    unittest.main()
