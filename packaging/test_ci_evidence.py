#!/usr/bin/env python3
"""CI 캐시와 구분되는 필수 검사 증거·동일 SHA 재사용의 신뢰 경계 회귀."""

import copy
from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest
from unittest.mock import patch

from ci_evidence import CANDIDATE_BRANCH, CHECKS, Evidence, EvidenceError, Pending, main

SHA = "a" * 40
NOW = datetime(2026, 10, 21, 12, tzinfo=timezone.utc)
REPO = "owner/project"


class Fixture:
    def __init__(self, filename="ci-macos.yml", branch=CANDIDATE_BRANCH, identifier=100):
        self.filename = filename
        self.meta = {"id": 10, "path": f".github/workflows/{filename}"}
        self.run = {"id": identifier, "workflow_id": 10, "run_attempt": 2,
                    "path": self.meta["path"], "head_sha": SHA, "head_branch": branch,
                    "repository": {"full_name": REPO}, "head_repository": {"full_name": REPO},
                    "event": "workflow_dispatch" if filename == "release.yml" else "push",
                    "updated_at": NOW.isoformat(), "status": "completed", "conclusion": "success"}
        self.runs = [self.run]
        self.jobs = [{"name": name, "run_id": identifier, "run_attempt": 2, "head_sha": SHA,
                      "status": "completed", "conclusion": "success"} for name in CHECKS[filename]]
        if filename == "release.yml":
            self.jobs += [{"name": name, "run_id": identifier, "run_attempt": 2, "head_sha": SHA,
                           "status": "completed", "conclusion": "skipped"}
                          for name in ("release", "publish-windows")]
        self.calls = []

    def read(self, endpoint, paginate=False):
        self.calls.append(endpoint)
        if endpoint.endswith(f"/workflows/{self.filename}"):
            return copy.deepcopy(self.meta)
        if "/workflows/10/runs?" in endpoint:
            return [{"workflow_runs": copy.deepcopy(self.runs)}]
        if f"/runs/{self.run['id']}/attempts/{self.run['run_attempt']}/jobs?" in endpoint:
            return [{"jobs": copy.deepcopy(self.jobs)}]
        raise AssertionError(f"unexpected API request: {endpoint}")

    def evidence(self):
        return Evidence(REPO, read=self.read, now=NOW)


class ProofTests(unittest.TestCase):
    def test_full_positive_proof_is_bound_to_exact_sha_workflow_scope_and_attempt(self):
        for filename in CHECKS:
            fixture = Fixture(filename)
            proof = fixture.evidence().proof(filename, SHA, CANDIDATE_BRANCH)
            self.assertEqual(proof["mode"], "full")
            self.assertEqual((proof["run_id"], proof["attempt"]), (100, 2))
            self.assertEqual(set(proof["jobs"]), CHECKS[filename])
            self.assertTrue(any("/attempts/2/jobs" in call for call in fixture.calls))

    def test_wrong_repository_path_sha_branch_workflow_and_event_are_rejected(self):
        changes = [("head_sha", "b" * 40), ("head_branch", "fork"), ("path", "untrusted/ci-macos.yml"),
                   ("workflow_id", 11), ("repository", {"full_name": "attacker/fork"}),
                   ("head_repository", {"full_name": "attacker/fork"}), ("event", "pull_request")]
        for field, value in changes:
            with self.subTest(field=field):
                fixture = Fixture()
                fixture.run[field] = value
                with self.assertRaises(EvidenceError):
                    fixture.evidence().proof(fixture.filename, SHA, CANDIDATE_BRANCH)

    def test_latest_pending_failed_cancelled_or_timed_out_attempt_is_not_replaced_by_an_old_success(self):
        for conclusion in (None, "failure", "cancelled", "timed_out", "skipped"):
            with self.subTest(conclusion=conclusion):
                fixture = Fixture()
                older = copy.deepcopy(fixture.run)
                older["id"] = 99
                fixture.runs.insert(0, older)
                fixture.run["conclusion"] = conclusion
                fixture.run["status"] = "in_progress" if conclusion is None else "completed"
                with self.assertRaises(EvidenceError):
                    fixture.evidence().proof(fixture.filename, SHA, CANDIDATE_BRANCH)

    def test_no_runs_is_pending_and_stale_evidence_is_rejected(self):
        fixture = Fixture()
        fixture.runs.clear()
        with self.assertRaises(Pending):
            fixture.evidence().proof(fixture.filename, SHA, CANDIDATE_BRANCH)
        fixture = Fixture()
        fixture.run["updated_at"] = (NOW - timedelta(hours=25)).isoformat()
        with self.assertRaisesRegex(EvidenceError, "24-hour"):
            fixture.evidence().proof(fixture.filename, SHA, CANDIDATE_BRANCH)

    def test_aggregate_success_without_actual_arm_job_is_not_full_validation(self):
        fixture = Fixture()
        fixture.jobs = [job for job in fixture.jobs if job["name"] in ("changes", "ci-macos")]
        with self.assertRaisesRegex(EvidenceError, "required validation jobs"):
            fixture.evidence().proof(fixture.filename, SHA, CANDIDATE_BRANCH)

    def test_packaging_must_succeed_even_when_other_required_jobs_are_green(self):
        for conclusion in (None, "failure", "cancelled", "skipped", "timed_out", "missing"):
            with self.subTest(conclusion=conclusion):
                fixture = Fixture()
                packaging = next(job for job in fixture.jobs
                                 if job["name"] == "packaging regressions (macos-14)")
                if conclusion == "missing":
                    fixture.jobs.remove(packaging)
                else:
                    packaging.update(conclusion=conclusion,
                                     status="in_progress" if conclusion is None else "completed")
                with self.assertRaisesRegex(EvidenceError, "required validation jobs"):
                    fixture.evidence().proof(fixture.filename, SHA, CANDIDATE_BRANCH)

    def test_skipped_wrong_attempt_and_duplicate_jobs_are_rejected(self):
        for mutation in ("skipped", "attempt", "duplicate", "sha"):
            with self.subTest(mutation=mutation):
                fixture = Fixture()
                if mutation == "skipped":
                    fixture.jobs[0]["conclusion"] = "skipped"
                elif mutation == "attempt":
                    fixture.jobs[0]["run_attempt"] = 1
                elif mutation == "sha":
                    fixture.jobs[0]["head_sha"] = "b" * 40
                else:
                    fixture.jobs.append(copy.deepcopy(fixture.jobs[0]))
                with self.assertRaises(EvidenceError):
                    fixture.evidence().proof(fixture.filename, SHA, CANDIDATE_BRANCH)

    def test_candidate_cache_or_manual_probe_cannot_replace_a_successful_push_validation(self):
        fixture = Fixture()
        fixture.run["event"] = "workflow_dispatch"
        with self.assertRaisesRegex(EvidenceError, "candidate push"):
            fixture.evidence().reusable(fixture.filename, SHA)

    def test_native_build_cannot_publish_during_candidate_verification(self):
        fixture = Fixture("release.yml")
        fixture.jobs[-1]["conclusion"] = "success"
        with self.assertRaisesRegex(EvidenceError, "must not publish"):
            fixture.evidence().proof(fixture.filename, SHA, CANDIDATE_BRANCH)

    def test_main_reuse_revalidates_full_candidate_proof_instead_of_trusting_gate_name(self):
        candidate = Fixture()
        promoted = Fixture(branch="main", identifier=200)
        promoted.jobs = [{"name": "reuse-macos-ci", "conclusion": "success", "status": "completed",
                          "head_sha": SHA, "run_id": 200, "run_attempt": 2}]

        def read(endpoint, paginate=False):
            if "branch=main" in endpoint or "/runs/200/" in endpoint:
                return promoted.read(endpoint, paginate)
            return candidate.read(endpoint, paginate)

        evidence = Evidence(REPO, read=read, now=NOW)
        proof = evidence.proof(candidate.filename, SHA, "main", allow_reuse=True)
        self.assertEqual(proof["source"]["run_id"], 100)
        candidate.jobs = [job for job in candidate.jobs
                          if job["name"] != "packaging regressions (macos-14)"]
        with self.assertRaises(EvidenceError):
            evidence.proof(candidate.filename, SHA, "main", allow_reuse=True)


class MacosGateTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Execute the actual Actions gate with its default fail-fast shell semantics.
        workflow = (Path(__file__).resolve().parents[1] / ".github/workflows/ci-macos.yml").read_text()
        gate = workflow.split("\n  ci-macos:\n", 1)[1]
        cls.script = textwrap.dedent(gate.split("        run: |\n", 1)[1])

    def accepted(self, **overrides):
        env = dict(os.environ, CHANGES="success", PROOF="success", REUSED="false",
                   RUST_CHANGED="true", CHECK="success", PACKAGING="success")
        env.update(overrides)
        result = subprocess.run(["bash", "-e", "-c", self.script],
                                env=env, capture_output=True, text=True, timeout=5)
        return result.returncode == 0

    def test_both_jobs_must_succeed_or_both_be_deliberately_skipped(self):
        states = ("success", "failure", "cancelled", "skipped")
        for reused, changed in (("false", "true"), ("false", "false"), ("true", "true"), ("true", "false")):
            expected = "success" if (reused, changed) == ("false", "true") else "skipped"
            for check in states:
                for packaging in states:
                    with self.subTest(reused=reused, changed=changed, check=check, packaging=packaging):
                        self.assertEqual(self.accepted(REUSED=reused, RUST_CHANGED=changed,
                                                       CHECK=check, PACKAGING=packaging),
                                         check == packaging == expected)

    def test_failed_prerequisites_cannot_pass_through_a_docs_or_reuse_skip(self):
        for prerequisite in ("CHANGES", "PROOF"):
            for result in ("failure", "cancelled", "skipped", ""):
                for reused in ("false", "true"):
                    with self.subTest(prerequisite=prerequisite, result=result, reused=reused):
                        self.assertFalse(self.accepted(**{prerequisite: result}, REUSED=reused,
                                                       RUST_CHANGED="false", CHECK="skipped", PACKAGING="skipped"))

    def test_missing_or_unknown_decision_outputs_are_rejected(self):
        for output in ("REUSED", "RUST_CHANGED"):
            for value in ("", "unknown"):
                with self.subTest(output=output, value=value):
                    self.assertFalse(self.accepted(**{output: value}, CHECK="skipped", PACKAGING="skipped"))


class ReuseCommandTests(unittest.TestCase):
    def test_missing_or_unverifiable_evidence_falls_back_to_full_ci_and_pr_never_reuses(self):
        for event, reference in (("push", "refs/heads/main"), ("pull_request", "refs/pull/1/merge")):
            with tempfile.TemporaryDirectory() as temporary:
                output = Path(temporary) / "output"
                env = {"GITHUB_EVENT_NAME": event, "GITHUB_REF": reference, "GITHUB_OUTPUT": str(output)}
                with patch.dict(os.environ, env), patch("ci_evidence.Evidence.reusable", side_effect=EvidenceError("offline")) as mocked:
                    with patch("sys.argv", ["ci_evidence.py", "reuse", "--repo", REPO, "--sha", SHA, "--workflow", "ci.yml"]):
                        main()
                self.assertEqual(output.read_text(), "reuse=false\n")
                self.assertEqual(mocked.call_count, 1 if event == "push" else 0)


if __name__ == "__main__":
    unittest.main()
