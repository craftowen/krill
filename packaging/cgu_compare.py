#!/usr/bin/env python3
"""Collect and compare manual CGU experiments; never select a release configuration."""

import argparse
from collections import Counter
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET

CASES = ("default", "32", "64")
STAGES = ("fmt, supply chain and notices", "clippy", "nextest", "doctest")


def command(*args):
    return subprocess.check_output(args, text=True, timeout=30).strip()


def prepare(directory, case):
    directory.mkdir(parents=True, exist_ok=True)
    if command("git", "status", "--porcelain", "--untracked-files=normal"):
        raise ValueError("comparison requires a clean source checkout")
    digest = hashlib.sha256()
    files = sorted(Path("target/debug/.fingerprint").rglob("*"))
    count = 0
    for path in files:
        if path.is_file():
            name, body = path.as_posix().encode(), path.read_bytes()
            digest.update(len(name).to_bytes(8, "big") + name)
            digest.update(len(body).to_bytes(8, "big") + body)
            count += 1
    metadata = {
        "case": case, "sha": command("git", "rev-parse", "HEAD"),
        "tree": command("git", "rev-parse", "HEAD^{tree}"),
        "run_id": os.environ["GITHUB_RUN_ID"], "attempt": os.environ["GITHUB_RUN_ATTEMPT"],
        "rustc": command("rustc", "-Vv"), "cargo": command("cargo", "--version"),
        "nextest": command("cargo", "nextest", "--version"),
        "system": [platform.machine(), platform.mac_ver()[0], os.cpu_count(),
                   command("sysctl", "-n", "hw.memsize"), command("sysctl", "-n", "hw.model")],
        "sdk": command("xcrun", "--show-sdk-version"),
        "image": [os.environ.get("ImageOS"), os.environ.get("ImageVersion")],
        "test_profile": [os.environ.get("CARGO_PROFILE_TEST_OPT_LEVEL"),
                         os.environ.get("CARGO_PROFILE_TEST_DEBUG")],
        "cache": [os.environ.get("RESTORED_CACHE_HIT"), count, digest.hexdigest()],
    }
    (directory / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")


def collect(directory):
    directory.mkdir(parents=True, exist_ok=True)
    for source, name in ((Path("target/nextest/ci/junit.xml"), "junit.xml"),
                         (Path("target/cargo-timings/cargo-timing.html"), "cargo-timing.html")):
        if source.is_file():
            shutil.copy2(source, directory / name)
    (directory / "source-status.txt").write_text(command("git", "status", "--porcelain") + "\n")


def read_measurement(directory):
    meta = json.loads((directory / "metadata.json").read_text())
    if meta["case"] not in CASES or meta["test_profile"] != ["2", "line-tables-only"]:
        raise ValueError("unexpected comparison profile")
    if (directory / "source-status.txt").read_text().strip():
        raise ValueError("source changed during measurement")
    inventory = Counter()
    tree = ET.parse(directory / "junit.xml")
    for suite in tree.iter("testsuite"):
        for case in suite.findall("testcase"):
            if case.find("failure") is not None or case.find("error") is not None:
                raise ValueError("failed test in measurement")
            inventory[(suite.get("name"), case.get("classname"), case.get("name"),
                       case.find("skipped") is not None)] += 1
    if not inventory:
        raise ValueError("empty test inventory")
    html = (directory / "cargo-timing.html").read_text()
    match = re.search(r"const UNIT_DATA = (\[.*?\]);", html, re.S)
    if not match:
        raise ValueError("missing Cargo unit timings")
    units = json.loads(match[1])
    signatures = Counter((u["name"], u["version"], u["target"], tuple(sorted(u["features"])))
                         for u in units)
    seconds = max(u["start"] + u["duration"] for u in units)
    if seconds <= 0:
        raise ValueError("empty compilation measurement")
    log = re.sub(r"(?:\x1b|\^\[)\[[0-9;]*m", "", (directory / "nextest.log").read_text())
    invocations = [line for line in log.splitlines()
                   if "Running `" in line and "--crate-name krill_app " in line]
    if not invocations:
        raise ValueError("missing actual app rustc invocations")
    for line in invocations:
        # Ignore environment assignments before rustc's crate arguments.
        arguments = line[line.index("--crate-name krill_app "):]
        def flags(name):
            return re.findall(r"(?:^|\s)-C\s*" + name + r"=([^\s\x60]+)", arguments)
        for name, expected in (("opt-level", "2"), ("debuginfo", "line-tables-only"),
                               ("debug-assertions", "on")):
            if set(flags(name)) != {expected}:
                raise ValueError("app optimization/assertion flags changed")
        cgu = set(flags("codegen-units"))
        if len(cgu) > 1 or (meta["case"] != "default" and cgu != {meta["case"]}):
            raise ValueError("app CGU override was not applied")
    summary = re.search(r"Summary[^\n]*", log)
    if not summary:
        raise ValueError("missing nextest summary")
    skipped = re.search(r"\b(\d+) skipped", summary[0])
    return {"meta": meta, "inventory": inventory, "signatures": signatures,
            "compile": seconds, "skipped": int(skipped[1]) if skipped else 0}


def compare(directory, jobs):
    paths = list(directory.rglob("metadata.json"))
    measurements = [read_measurement(path.parent) for path in paths]
    cases = {m["meta"]["case"]: m for m in measurements}
    if len(measurements) != 3 or set(cases) != set(CASES):
        raise ValueError("all three unique CGU measurements are required")
    baseline = cases["default"]
    common = ("sha", "tree", "run_id", "attempt", "rustc", "cargo", "nextest",
              "system", "sdk", "image", "test_profile", "cache")
    rows = []
    for case in CASES:
        value = cases[case]
        if any(value["meta"][key] != baseline["meta"][key] for key in common):
            raise ValueError("source, toolchain, machine or restored cache differs")
        if any(value[key] != baseline[key] for key in ("inventory", "signatures", "skipped")):
            raise ValueError("test inventory, skipped tests or Cargo features differ")
        matches = [j for j in jobs["jobs"] if j["name"] == f"cgu-{case} (macos-14)"]
        if len(matches) != 1:
            raise ValueError("missing or ambiguous workflow job")
        job = matches[0]
        meta = value["meta"]
        if (str(job["run_id"]) != meta["run_id"] or str(job["run_attempt"]) != meta["attempt"]
                or job["head_sha"] != meta["sha"] or job["conclusion"] != "success"):
            raise ValueError("job identity or result does not match evidence")
        for stage in STAGES:
            found = [s for s in job["steps"] if s["name"] == stage]
            if len(found) != 1 or found[0]["conclusion"] != "success":
                raise ValueError("required validation stage did not succeed")
        parse = lambda date: datetime.fromisoformat(date.replace("Z", "+00:00"))
        elapsed = (parse(job["completed_at"]) - parse(job["started_at"])).total_seconds()
        rows.append((case, value["compile"], elapsed, sum(value["inventory"].values()), value["skipped"]))
    lines = ["## App test CGU comparison", "", f"Source: `{baseline['meta']['sha']}`", "",
             "| CGU | Cargo compile (s) | Job elapsed (s) | Test cases | Ignored/skipped summary |",
             "|---|---:|---:|---:|---:|"]
    lines += [f"| {case} | {compile_seconds:.2f} | {elapsed:.0f} | {tests} | {skipped} |"
              for case, compile_seconds, elapsed, tests, skipped in rows]
    lines += ["", "All three jobs passed the same tests and validation stages. Restored fingerprints and reported machine/toolchain match.",
              "One observation per setting; repeat close results. This report does not adopt a setting or serve as release validation."]
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("prepare", "collect", "compare"))
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--case", choices=CASES)
    parser.add_argument("--jobs", type=Path)
    args = parser.parse_args()
    if args.action == "prepare":
        if args.case is None:
            parser.error("prepare requires --case")
        prepare(args.directory, args.case)
    elif args.action == "collect":
        collect(args.directory)
    else:
        if args.jobs is None:
            parser.error("compare requires --jobs")
        try:
            print(compare(args.directory, json.loads(args.jobs.read_text())), end="")
        except (ValueError, KeyError, OSError, ET.ParseError) as error:
            print(f"Comparison incomplete or uncontrolled: {error}")
            sys.exit(1)


if __name__ == "__main__":
    main()
