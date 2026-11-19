#!/usr/bin/env python3
"""GitHub의 동일 SHA·저장소·workflow·attempt·실제 잡 성공을 확인하고, CI의 FLAKY 통과를 보고한다."""

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
from urllib.parse import urlencode
import xml.etree.ElementTree as ET

CANDIDATE_BRANCH = "release/nightly"
CANDIDATE_PREFIX = "release/"
CHECKS = {
    "ci.yml": {"hygiene"},
    "ci-macos.yml": {"changes", "ci-macos", "clippy · nextest (macos-15)",
                     "packaging regressions (macos-15)"},
    "ci-windows.yml": {"check (MSVC)", "nextest (MSVC)", "release app (fxc shaders)"},
    "release.yml": {"build-windows"},
}
REUSE_JOBS = {"ci.yml": "reuse-hygiene-ci", "ci-macos.yml": "reuse-macos-ci"}


class EvidenceError(RuntimeError):
    pass


class Pending(EvidenceError):
    def __init__(self, message, run=None):
        super().__init__(message)
        self.run = run


class FailedRun(EvidenceError):
    def __init__(self, run):
        super().__init__(f"run {run['id']}: {run.get('conclusion')}")
        self.run = run


def gh_json(endpoint, paginate=False):
    args = ["gh", "api", endpoint]
    if paginate:
        args.extend(("--paginate", "--slurp"))
    # 읽기만 재시도한다. 테스트 실패나 원격 쓰기는 여기서 실행하지 않는다.
    for attempt in range(3):
        try:
            result = subprocess.run(args, capture_output=True, text=True, timeout=120)
        except subprocess.TimeoutExpired as error:
            if attempt == 2:
                raise EvidenceError(f"GitHub evidence read timed out: {endpoint}") from error
            time.sleep(2 ** attempt)
            continue
        if result.returncode == 0:
            return json.loads(result.stdout)
        if attempt == 2:
            raise EvidenceError(f"GitHub evidence read failed: {endpoint}: {result.stderr.strip()}")
        time.sleep(2 ** attempt)
    raise EvidenceError("unreachable GitHub read state")


class Evidence:
    def __init__(self, repository, read=gh_json, now=None):
        if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
            raise EvidenceError("invalid repository")
        self.repository = repository
        self.read = read
        self.now = now
        self.workflows = {}

    def workflow(self, filename):
        if filename not in CHECKS:
            raise EvidenceError("unknown required workflow")
        if filename not in self.workflows:
            data = self.read(f"repos/{self.repository}/actions/workflows/{filename}")
            if data.get("path") != f".github/workflows/{filename}" or type(data.get("id")) is not int:
                raise EvidenceError("workflow identity does not match")
            self.workflows[filename] = data["id"]
        return self.workflows[filename]

    def run(self, filename, sha, branch):
        if not re.fullmatch(r"[0-9a-f]{40}", sha):
            raise EvidenceError("invalid source SHA")
        workflow_id = self.workflow(filename)
        query = urlencode({"head_sha": sha, "branch": branch, "per_page": 100})
        pages = self.read(f"repos/{self.repository}/actions/workflows/{workflow_id}/runs?{query}", paginate=True)
        runs = [run for page in pages for run in page["workflow_runs"]]
        if not runs:
            raise Pending(f"waiting for {filename} at {sha}")
        run = max(runs, key=lambda item: (item["id"], item["run_attempt"]))
        expected = (run.get("head_sha") == sha and run.get("head_branch") == branch
                    and run.get("path") == f".github/workflows/{filename}"
                    and run.get("workflow_id") == workflow_id
                    and run.get("repository", {}).get("full_name") == self.repository
                    and run.get("head_repository", {}).get("full_name") == self.repository
                    and type(run.get("id")) is int and type(run.get("run_attempt")) is int
                    and run["run_attempt"] >= 1)
        if not expected:
            raise EvidenceError(f"{filename}: run identity does not match the candidate")
        allowed = {"workflow_dispatch"} if filename == "release.yml" else {"push", "workflow_dispatch"}
        if run.get("event") not in allowed:
            raise EvidenceError(f"{filename}: untrusted run event")
        completed = datetime.fromisoformat(run["updated_at"].replace("Z", "+00:00"))
        age = ((self.now or datetime.now(timezone.utc)) - completed).total_seconds()
        if age < -300 or age > 86400:
            raise EvidenceError(f"{filename}: evidence is outside the 24-hour validity window")
        if run.get("status") != "completed":
            raise Pending(f"waiting for {filename} run {run['id']}", run)
        if run.get("conclusion") != "success":
            raise FailedRun(run)
        return run

    def proof(self, filename, sha, branch, allow_reuse=False):
        run = self.run(filename, sha, branch)
        endpoint = f"repos/{self.repository}/actions/runs/{run['id']}/attempts/{run['run_attempt']}/jobs?per_page=100"
        pages = self.read(endpoint, paginate=True)
        jobs = [job for page in pages for job in page["jobs"]]
        if any(job.get("run_id") != run["id"] or job.get("head_sha") != sha
               or job.get("run_attempt") != run["run_attempt"] for job in jobs):
            raise EvidenceError("job identity does not match the verified run")
        names = {}
        for job in jobs:
            if job["name"] in names:
                raise EvidenceError("duplicate job names cannot prove validation scope")
            names[job["name"]] = job
        full = all(names.get(name, {}).get("status") == "completed"
                   and names[name].get("conclusion") == "success" for name in CHECKS[filename])
        proof = {"workflow": filename, "sha": sha, "branch": branch, "run_id": run["id"],
                 "attempt": run["run_attempt"], "mode": "full", "jobs": sorted(CHECKS[filename])}
        if full:
            if filename == "release.yml" and any(names.get(name, {}).get("conclusion") != "skipped"
                                                   for name in ("release", "publish-windows")):
                raise EvidenceError("native candidate build must not publish")
            return proof
        reuse_name = REUSE_JOBS.get(filename)
        if allow_reuse and branch == "main" and reuse_name and names.get(reuse_name, {}).get("conclusion") == "success":
            proof.update(mode="reused", source=self.reusable(filename, sha))
            return proof
        raise EvidenceError(f"{filename}: missing, skipped, or unsuccessful required validation jobs")

    def reusable(self, filename, sha):
        # Every release/** push runs full CI (workflow trigger), so any of them is a candidate.
        # The newest one decides: a later failure on another release branch is not hidden.
        query = urlencode({"head_sha": sha, "per_page": 100})
        pages = self.read(f"repos/{self.repository}/actions/workflows/{self.workflow(filename)}/runs?{query}",
                          paginate=True)
        candidates = [run for page in pages for run in page["workflow_runs"]
                      if str(run.get("head_branch")).startswith(CANDIDATE_PREFIX)]
        if not candidates:
            raise Pending(f"waiting for {filename} at {sha} on a {CANDIDATE_PREFIX} branch")
        branch = max(candidates, key=lambda item: (item["id"], item["run_attempt"]))["head_branch"]
        proof = self.proof(filename, sha, branch)
        if self.run(filename, sha, branch)["event"] != "push":
            raise EvidenceError("reused validation must come from a candidate push")
        return proof


def flaky(junit, known):
    """Tests that passed only on retry (nextest JUnit flakyFailure), and those not in the known list."""
    found = sorted({f"{case.get('classname')} {case.get('name')}" for case in ET.parse(junit).iter("testcase")
                    if case.find("flakyFailure") is not None})
    allowed = {line.split("#", 1)[0].strip() for line in Path(known).read_text().splitlines()}
    return found, [test for test in found if test not in allowed]


def report_flaky(junit, known):
    found, new = flaky(junit, known)
    for test in found:
        print(f"::{'error' if test in new else 'warning'} title=FLAKY::{test}")
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a") as stream:
            stream.write(f"### FLAKY (passed only on retry): {len(found)}, new: {len(new)}\n")
            stream.writelines(f"- {'**new** ' if test in new else ''}`{test}`\n" for test in found)
    if new:
        print(f"{len(new)} new FLAKY test(s); fix them or add them to {known} with a reason", file=sys.stderr)
    return int(bool(new))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("reuse", "verify", "flaky"))
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY"), required=False)
    parser.add_argument("--sha", default=os.environ.get("GITHUB_SHA"), required=False)
    parser.add_argument("--branch", default="main")
    parser.add_argument("--workflow", choices=tuple(REUSE_JOBS))
    parser.add_argument("--junit", type=Path, default=Path("target/nextest/ci/junit.xml"))
    parser.add_argument("--known", type=Path, default=Path(".config/known-flaky.txt"))
    args = parser.parse_args()
    if args.action == "flaky":
        sys.exit(report_flaky(args.junit, args.known))
    evidence = Evidence(args.repo or "")
    if args.action == "reuse":
        proof = None
        if os.environ.get("GITHUB_EVENT_NAME") == "push" and os.environ.get("GITHUB_REF") == "refs/heads/main":
            try:
                proof = evidence.reusable(args.workflow, args.sha or "")
            except (EvidenceError, ValueError, KeyError, TypeError) as error:
                print(f"Full validation required: {error}", file=sys.stderr)
        if os.environ.get("GITHUB_OUTPUT"):
            with Path(os.environ["GITHUB_OUTPUT"]).open("a") as stream:
                stream.write(f"reuse={'true' if proof else 'false'}\n")
        if proof and os.environ.get("GITHUB_STEP_SUMMARY"):
            source = f"https://github.com/{args.repo}/actions/runs/{proof['run_id']}"
            with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a") as stream:
                stream.write(f"Same SHA `{args.sha}`: reused [candidate CI]({source}), attempt {proof['attempt']}.\n")
        print(json.dumps(proof, ensure_ascii=False))
    else:
        proofs = [evidence.proof(filename, args.sha or "", args.branch, allow_reuse=True) for filename in REUSE_JOBS]
        print(json.dumps(proofs, ensure_ascii=False))


if __name__ == "__main__":
    main()
