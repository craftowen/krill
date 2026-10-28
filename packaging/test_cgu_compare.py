#!/usr/bin/env python3
"""Reject incomplete or uncontrolled CGU comparisons instead of reporting a speedup."""

import copy
import json
from pathlib import Path
import tempfile
import unittest

from cgu_compare import CASES, STAGES, compare


class ComparisonTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.jobs = {"jobs": []}
        for case in CASES:
            folder = self.root / case
            folder.mkdir()
            metadata = {"case": case, "sha": "a" * 40, "tree": "b" * 40,
                        "run_id": "10", "attempt": "2", "rustc": "1.98.1", "cargo": "1.98.1",
                        "nextest": "same", "system": ["arm64", "14", 3, "7GB", "VirtualMac"],
                        "sdk": "same", "image": ["macos14", "same"],
                        "test_profile": ["2", "line-tables-only"], "cache": ["true", 100, "same"]}
            (folder / "metadata.json").write_text(json.dumps(metadata))
            (folder / "source-status.txt").write_text("")
            (folder / "junit.xml").write_text('<testsuites><testsuite name="app"><testcase classname="x" name="a"/></testsuite></testsuites>')
            units = [{"name": "app", "version": "1", "target": " lib(test)", "features": [],
                      "start": 10, "duration": 100 if case == "default" else 90}]
            (folder / "cargo-timing.html").write_text("const UNIT_DATA = " + json.dumps(units) + ";")
            cgu = "" if case == "default" else f" -C codegen-units={case}"
            (folder / "nextest.log").write_text(
                "Running `rustc --crate-name krill_app --edition=2024 -C opt-level=2"
                f" -C debuginfo=line-tables-only -C debug-assertions=on{cgu} --test`\n"
                "Summary [ 1s] 1 tests run: 1 passed, 17 skipped\n")
            self.jobs["jobs"].append({"name": f"cgu-{case} (macos-14)", "run_id": 10,
                                     "run_attempt": 2, "head_sha": "a" * 40, "conclusion": "success",
                                     "started_at": "2026-10-25T00:00:00Z", "completed_at": "2026-10-25T00:03:00Z",
                                     "steps": [{"name": name, "conclusion": "success"} for name in STAGES]})

    def modify_metadata(self, **values):
        path = self.root / "32/metadata.json"
        data = json.loads(path.read_text())
        data.update(values)
        path.write_text(json.dumps(data))

    def test_complete_matched_comparison_reports_measurements_without_adopting_a_setting(self):
        report = compare(self.root, self.jobs)
        self.assertIn("| default | 110.00 |", report)
        self.assertIn("| 32 | 100.00 |", report)
        self.assertIn("does not adopt a setting", report)

    def test_source_cache_machine_and_profile_mismatches_are_rejected(self):
        path = self.root / "32/metadata.json"
        original = path.read_text()
        for field, value in (("sha", "c" * 40), ("attempt", "3"), ("rustc", "other"),
                             ("system", ["other"]), ("cache", ["true", 99, "other"]),
                             ("test_profile", ["0", "line-tables-only"])):
            with self.subTest(field=field):
                path.write_text(original)
                self.modify_metadata(**{field: value})
                with self.assertRaises(ValueError):
                    compare(self.root, self.jobs)

    def test_equal_test_counts_with_different_names_do_not_pass(self):
        path = self.root / "32/junit.xml"
        path.write_text(path.read_text().replace('name="a"', 'name="replacement"'))
        with self.assertRaisesRegex(ValueError, "inventory"):
            compare(self.root, self.jobs)

    def test_changed_features_are_rejected(self):
        path = self.root / "32/cargo-timing.html"
        path.write_text(path.read_text().replace('"features": []', '"features": ["other"]'))
        with self.assertRaisesRegex(ValueError, "features"):
            compare(self.root, self.jobs)

    def test_missing_one_of_two_identical_unit_signatures_is_rejected(self):
        for case in CASES:
            path = self.root / case / "cargo-timing.html"
            unit = json.loads(path.read_text().split(" = ", 1)[1][:-1])[0]
            units = [unit, copy.deepcopy(unit)] if case != "32" else [unit]
            path.write_text("const UNIT_DATA = " + json.dumps(units) + ";")
        with self.assertRaisesRegex(ValueError, "features"):
            compare(self.root, self.jobs)

    def test_failed_validation_job_or_wrong_attempt_is_rejected(self):
        for change in ("stage", "job", "attempt", "missing", "duplicate"):
            with self.subTest(change=change):
                jobs = copy.deepcopy(self.jobs)
                if change == "stage":
                    jobs["jobs"][1]["steps"][0]["conclusion"] = "failure"
                elif change == "job":
                    jobs["jobs"][1]["conclusion"] = "cancelled"
                elif change == "attempt":
                    jobs["jobs"][1]["run_attempt"] = 1
                elif change == "missing":
                    jobs["jobs"].pop()
                else:
                    jobs["jobs"].append(copy.deepcopy(jobs["jobs"][0]))
                with self.assertRaises(ValueError):
                    compare(self.root, jobs)

    def test_missing_or_wrong_actual_rustc_flags_are_rejected(self):
        path = self.root / "32/nextest.log"
        original = path.read_text()
        for text in (original.replace("codegen-units=32", "codegen-units=64"),
                     original.replace("debug-assertions=on", "debug-assertions=off"),
                     original.replace("opt-level=2", "opt-level=0"), "Summary 1 passed, 17 skipped"):
            with self.subTest(text=text):
                path.write_text(text)
                with self.assertRaises(ValueError):
                    compare(self.root, self.jobs)

    def test_conflicting_duplicate_rustc_options_are_rejected(self):
        path = self.root / "32/nextest.log"
        original = path.read_text()
        for flag in ("codegen-units=16", "opt-level=0", "debug-assertions=off", "debuginfo=2"):
            with self.subTest(flag=flag):
                path.write_text(original.replace(" --test", f" -C {flag} --test"))
                with self.assertRaises(ValueError):
                    compare(self.root, self.jobs)

    def test_changed_source_and_duplicate_measurement_are_rejected(self):
        status = self.root / "32/source-status.txt"
        status.write_text(" M Cargo.toml\n")
        with self.assertRaisesRegex(ValueError, "source changed"):
            compare(self.root, self.jobs)
        status.write_text("")
        import shutil
        shutil.copytree(self.root / "32", self.root / "duplicate")
        with self.assertRaisesRegex(ValueError, "unique"):
            compare(self.root, self.jobs)

    def test_failure_empty_or_missing_test_evidence_is_rejected(self):
        path = self.root / "32/junit.xml"
        for content in ('<testsuites/>', '<testsuites><testsuite name="app"><testcase name="a"><failure/></testcase></testsuite></testsuites>'):
            with self.subTest(content=content):
                path.write_text(content)
                with self.assertRaises(ValueError):
                    compare(self.root, self.jobs)
        path.unlink()
        with self.assertRaises(FileNotFoundError):
            compare(self.root, self.jobs)

    def test_zero_ignored_tests_is_a_valid_summary(self):
        for case in CASES:
            path = self.root / case / "nextest.log"
            path.write_text(path.read_text().replace(", 17 skipped", ""))
        self.assertIn("| default |", compare(self.root, self.jobs))


if __name__ == "__main__":
    unittest.main()
