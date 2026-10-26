#!/usr/bin/env python3
"""Stage immutable path dependencies and audit isolated macOS cache experiments."""

import argparse
from collections import Counter
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import xml.etree.ElementTree as ET

from cgu_compare import collect as collect_common, prepare as prepare_common

CASES = ("control", "producer", "consumer")
STAGES = ("fmt, supply chain and notices", "clippy", "nextest", "Test inventory",
          "doctest", "Native packaging regressions", "Collect evidence")
HELPERS = {"krill", "krill-holder", "krilld", "krill-link"}


def digest_tree(root):
    digest = hashlib.sha256()
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise ValueError("vendor symlinks require an explicit cache policy")
        mode = stat.S_IMODE(path.stat().st_mode)
        name = path.relative_to(root).as_posix().encode()
        body = path.read_bytes() if path.is_file() else b""
        kind = b"f" if path.is_file() else b"d"
        digest.update(kind + mode.to_bytes(4, "big") + len(name).to_bytes(8, "big") + name)
        digest.update(len(body).to_bytes(8, "big") + body)
    return digest.hexdigest()


def patches(root):
    text = (root / "Cargo.toml").read_text()
    section = text.split("[patch.crates-io]\n", 1)[1].split("\n[", 1)[0]
    result = {}
    for line in section.splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        match = re.fullmatch(r'([\w-]+)\s*=\s*\{\s*path\s*=\s*"vendor/([\w-]+)"\s*\}', line)
        if not match or match[1] in result:
            raise ValueError("unsupported or duplicate vendor patch declaration")
        result[match[1]] = match[2]
    if not result:
        raise ValueError("no vendor patches")
    return result


def stage(root, cargo_home, vendor_root, directory, case):
    root, cargo_home, vendor_root = root.resolve(), cargo_home.resolve(), vendor_root.resolve()
    for outside in (cargo_home, vendor_root):
        if outside == root or root in outside.parents:
            raise ValueError("Cargo home and staged vendor must be outside the source checkout")
    mapping = patches(root)
    vendor_hash = digest_tree(root / "vendor")
    destination = vendor_root / vendor_hash / "vendor"
    config_hash = None
    if case != "control":
        if destination.exists():
            if digest_tree(destination) != vendor_hash:
                raise ValueError("existing staged vendor content or modes differ")
        else:
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copytree(root / "vendor", destination, copy_function=shutil.copy2)
        config = "# Generated CI-only vendor cache configuration.\n[patch.crates-io]\n"
        config += "".join(f'{name} = {{ path = {json.dumps(str(destination / relative))} }}\n'
                          for name, relative in sorted(mapping.items()))
        cargo_home.mkdir(parents=True, exist_ok=True)
        config_path = cargo_home / "config.toml"
        if (cargo_home / "config").exists() or (config_path.exists() and config_path.read_text() != config):
            raise ValueError("refusing to replace an existing Cargo home configuration")
        config_path.write_text(config)
        config_hash = hashlib.sha256(config.encode()).hexdigest()
    directory.mkdir(parents=True, exist_ok=True)
    record = {"case": case, "vendor_hash": vendor_hash, "patches": mapping,
              "vendor_directory": str(destination), "cargo_home": str(cargo_home), "config_sha256": config_hash}
    (directory / "vendor.json").write_text(json.dumps(record, indent=2) + "\n")
    return record


def normalized_graph(metadata):
    identities = {p["id"]: (p["name"], p["version"], p["source"] or "path") for p in metadata["packages"]}
    if len(set(identities.values())) != len(identities):
        raise ValueError("ambiguous normalized package identities")
    nodes = sorted((identities[n["id"]], tuple(sorted(n["features"])),
                    tuple(sorted((d["name"], identities[d["pkg"]], json.dumps(d["dep_kinds"], sort_keys=True))
                                 for d in n["deps"]))) for n in metadata["resolve"]["nodes"])
    return {"members": sorted(identities[p] for p in metadata["workspace_members"]), "nodes": nodes}


def verify_stage(directory):
    record = json.loads((directory / "vendor.json").read_text())
    if record["case"] != "control":
        if digest_tree(Path(record["vendor_directory"])) != record["vendor_hash"]:
            raise ValueError("restored vendor differs from tracked source content or modes")
        config = Path(record["cargo_home"]) / "config.toml"
        if hashlib.sha256(config.read_bytes()).hexdigest() != record["config_sha256"]:
            raise ValueError("external Cargo configuration changed")
    if digest_tree(Path("vendor")) != record["vendor_hash"]:
        raise ValueError("tracked vendor changed")
    return record


def prepare(directory):
    record = verify_stage(directory)
    # Metadata runs through the same global config as rust-cache's own invocation.
    raw = subprocess.check_output(["cargo", "metadata", "--all-features", "--locked", "--format-version=1"],
                                  text=True, timeout=240)
    metadata = json.loads(raw)
    by_name = {p["name"]: p for p in metadata["packages"] if p["name"] in record["patches"]}
    for name, relative in record["patches"].items():
        base = Path("vendor").resolve() if record["case"] == "control" else Path(record["vendor_directory"])
        if Path(by_name[name]["manifest_path"]) != base / relative / "Cargo.toml":
            raise ValueError("Cargo metadata did not observe the intended vendor configuration")
    (directory / "cargo-metadata.json").write_text(raw)
    prepare_common(directory, record["case"])
    path = directory / "metadata.json"
    common = json.loads(path.read_text())
    common.update(vendor_hash=record["vendor_hash"],
                  baseline_cache_hit=os.environ.get("BASELINE_CACHE_HIT"),
                  vendor_cache_hit=os.environ.get("VENDOR_CACHE_HIT"),
                  graph_sha256=hashlib.sha256(json.dumps(normalized_graph(metadata), sort_keys=True).encode()).hexdigest())
    if record["case"] == "consumer" and common["vendor_cache_hit"] != "true":
        raise ValueError("consumer did not restore the exact producer cache")
    if record["case"] == "producer" and common["vendor_cache_hit"] == "true":
        raise ValueError("producer must start without its isolated cache")
    path.write_text(json.dumps(common, indent=2) + "\n")


def cargo_messages(log):
    # Cargo -vv also writes textual build-script output to stdout; do not use it.
    for line in log.splitlines():
        if not line.strip():
            continue
        value = json.loads(line)
        if not isinstance(value, dict) or "reason" not in value:
            raise ValueError("unexpected Cargo JSON message")
        yield value


def artifact_record(item, packages):
    package = packages[item["package_id"]]
    return {"package": [package["name"], package["version"], package["source"] or "path"],
            "target": [item["target"]["name"], item["target"]["kind"]],
            "features": sorted(item["features"]), "profile": item["profile"],
            "fresh": item["fresh"], "executable": item["executable"]}


def collect(directory):
    collect_common(directory)
    packages = {p["id"]: p for p in json.loads((directory / "cargo-metadata.json").read_text())["packages"]}
    artifacts, builds = [], []
    for item in cargo_messages((directory / "nextest-cargo.jsonl").read_text()):
        package = packages.get(item.get("package_id"))
        if item["reason"] == "compiler-artifact":
            if package is None:
                raise ValueError("artifact package missing from metadata")
            artifacts.append(artifact_record(item, packages))
        elif item["reason"] == "build-script-executed" and package and package["name"] == "krill-proto":
            builds.extend(value for name, value in item["env"] if name == "KRILL_DEV_BUILD")
    if not artifacts or len(set(builds)) != 1:
        raise ValueError("missing artifacts or development BUILD evidence")
    helpers = set()
    for item in artifacts:
        if (item["package"][0] in HELPERS and item["target"] == [item["package"][0], ["bin"]]
                and item["executable"] and not item["profile"]["test"]):
            if item["fresh"] or not os.access(item["executable"], os.X_OK):
                raise ValueError("normal helper must be freshly built and executable")
            helpers.add(item["package"][0])
    if helpers != HELPERS:
        raise ValueError("missing one of four freshly built normal helpers")
    clippy = [artifact_record(item, packages)
              for item in cargo_messages((directory / "clippy-cargo.jsonl").read_text())
              if item["reason"] == "compiler-artifact"]
    if not clippy:
        raise ValueError("missing clippy artifacts")
    (directory / "compilation.json").write_text(json.dumps({"artifacts": artifacts, "clippy_artifacts": clippy, "build": builds[0],
                                                           "helpers": sorted(helpers)}, indent=2) + "\n")


def inventory(listing):
    rows = []
    for binary_id, suite in listing["rust-suites"].items():
        if suite["status"] != "listed":
            raise ValueError("unlisted test suite")
        for name, case in suite["testcases"].items():
            rows.append((suite["package-name"], binary_id, name, case["ignored"],
                         json.dumps(case["filter-match"], sort_keys=True)))
    if len(rows) != listing["test-count"] or not rows:
        raise ValueError("incomplete test inventory")
    return Counter(rows)


def read_measurement(directory):
    meta = json.loads((directory / "metadata.json").read_text())
    if meta["case"] not in CASES or meta["test_profile"] != ["2", "line-tables-only"]:
        raise ValueError("unexpected experiment profile")
    if (directory / "source-status.txt").read_text().strip():
        raise ValueError("tracked source changed")
    listing = inventory(json.loads((directory / "test-list.json").read_text()))
    executed = Counter()
    for suite in ET.parse(directory / "junit.xml").iter("testsuite"):
        for case in suite.findall("testcase"):
            if case.find("failure") is not None or case.find("error") is not None or case.find("skipped") is not None:
                raise ValueError("failed or newly skipped executed test")
            executed[(suite.get("name"), case.get("classname"), case.get("name"))] += 1
    expected = Counter({(row[1], row[1], row[2]): count for row, count in listing.items()
                        if json.loads(row[-1])["status"] == "matches"})
    if not executed or executed != expected:
        raise ValueError("executed tests differ from full listing")
    compilation = json.loads((directory / "compilation.json").read_text())
    signatures = Counter()
    vendors = []
    names = json.loads((directory / "vendor.json").read_text())["patches"]
    all_artifacts = [("nextest", i) for i in compilation["artifacts"]]
    all_artifacts += [("clippy", i) for i in compilation["clippy_artifacts"]]
    for which, item in all_artifacts:
        if which == "nextest" and item["package"][0] == "krill-app" and "custom-build" not in item["target"][1]:
            profile = item["profile"]
            if (profile["opt_level"] != "2" or profile["debuginfo"] != "line-tables-only"
                    or not profile["debug_assertions"] or not profile["overflow_checks"]):
                raise ValueError("app optimization/assertion contract changed")
        signatures[(which, json.dumps({k: item[k] for k in ("package", "target", "features", "profile")}, sort_keys=True))] += 1
        if item["package"][0] in names:
            vendors.append(item)
    if not vendors or not compilation["build"] or set(compilation["helpers"]) != HELPERS:
        raise ValueError("missing vendor, BUILD or helpers evidence")
    if meta["case"] == "consumer" and any(not item["fresh"] for item in vendors):
        dirty = sorted({i["package"][0] for i in vendors if not i["fresh"]})
        raise ValueError("consumer rebuilt vendor: " + ", ".join(dirty))
    match = re.search(r"const UNIT_DATA = (\[.*?\]);", (directory / "cargo-timing.html").read_text(), re.S)
    if not match:
        raise ValueError("missing timing data")
    units = json.loads(match[1])
    seconds = max(u["start"] + u["duration"] for u in units)
    return {"meta": meta, "listing": listing, "executed": executed, "signatures": signatures,
            "build": compilation["build"], "compile": seconds,
            "vendor_units": len(vendors), "fresh_vendor_units": sum(i["fresh"] for i in vendors)}


def compare(directory, jobs):
    measurements = [read_measurement(p.parent) for p in directory.rglob("metadata.json")]
    cases = {m["meta"]["case"]: m for m in measurements}
    if len(measurements) != 3 or set(cases) != set(CASES):
        raise ValueError("three unique complete measurements required")
    baseline = cases["control"]
    common = ("sha", "tree", "run_id", "attempt", "rustc", "cargo", "nextest", "system", "sdk",
              "image", "test_profile", "vendor_hash", "graph_sha256")
    if cases["producer"]["meta"]["cache"] != baseline["meta"]["cache"]:
        raise ValueError("producer and control seed dependency caches differ")
    if cases["consumer"]["meta"]["vendor_cache_hit"] != "true":
        raise ValueError("consumer was not an exact isolated cache hit")
    rows = []
    for name in CASES:
        value = cases[name]
        if any(value["meta"][key] != baseline["meta"][key] for key in common):
            raise ValueError("source, toolchain, hardware, profile or graph differs")
        if any(value[key] != baseline[key] for key in ("listing", "executed", "signatures", "build")):
            raise ValueError("test IDs/skips, compiled features/profiles or BUILD differs")
        candidates = [j for j in jobs["jobs"] if j["name"].endswith(f"vendor-cache-{name} (macos-14)")]
        if len(candidates) != 1:
            raise ValueError("missing or ambiguous measurement job")
        job = candidates[0]
        if (job["head_sha"] != value["meta"]["sha"] or str(job["run_id"]) != value["meta"]["run_id"]
                or str(job["run_attempt"]) != value["meta"]["attempt"] or job["conclusion"] != "success"):
            raise ValueError("job identity or result differs")
        for name_stage in STAGES:
            found = [s for s in job["steps"] if s["name"] == name_stage]
            if len(found) != 1 or found[0]["conclusion"] != "success":
                raise ValueError("required stage did not succeed: " + name_stage)
        date = lambda text: datetime.fromisoformat(text.replace("Z", "+00:00"))
        elapsed = (date(job["completed_at"]) - date(job["started_at"])).total_seconds()
        queued = (date(job["started_at"]) - date(job["created_at"])).total_seconds()
        rows.append({"case": name, "compile_seconds": value["compile"], "job_seconds": elapsed,
                     "queue_seconds": queued, "gate_seconds": elapsed + queued,
                     "executed": sum(value["executed"].values()), "listed": sum(value["listing"].values()),
                     "vendor_units": value["vendor_units"], "fresh_vendor_units": value["fresh_vendor_units"]})
    lines = ["## Isolated vendor cache comparison", "", f"SHA: `{baseline['meta']['sha']}`", "",
             "| Case | Compile s | Job s (including post/save) | Queue s | Gate s | Tests | Fresh vendor units |",
             "|---|---:|---:|---:|---:|---:|---:|"]
    lines += [f"| {r['case']} | {r['compile_seconds']:.2f} | {r['job_seconds']:.2f} | {r['queue_seconds']:.2f} | {r['gate_seconds']:.2f} | {r['executed']} | {r['fresh_vendor_units']}/{r['vendor_units']} |" for r in rows]
    lines += ["", "Correctness matched. One producer/consumer/control observation does not adopt a production setting.",
              "Consumer dependency wait is excluded; its own runner queue is included. Repeat before attributing a durable gain."]
    return rows, "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("stage", "verify", "prepare", "collect", "compare"))
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--case", choices=CASES)
    parser.add_argument("--cargo-home", type=Path)
    parser.add_argument("--vendor-root", type=Path)
    parser.add_argument("--jobs", type=Path)
    args = parser.parse_args()
    if args.action == "stage":
        if not all((args.case, args.cargo_home, args.vendor_root)):
            parser.error("stage requires case, cargo-home and vendor-root")
        record = stage(Path.cwd(), args.cargo_home, args.vendor_root, args.directory, args.case)
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write(f"vendor-hash={record['vendor_hash']}\nvendor-directory={record['vendor_directory']}\n")
            if sys.platform == "darwin":
                sdk = subprocess.check_output(["xcrun", "--show-sdk-version"], text=True, timeout=30).strip()
                output.write(f"sdk={sdk}\nimage={os.environ.get('ImageOS', 'macos')}-{os.environ.get('ImageVersion', 'local')}\n")
    elif args.action == "verify":
        verify_stage(args.directory)
    elif args.action == "prepare":
        prepare(args.directory)
    elif args.action == "collect":
        collect(args.directory)
    else:
        if args.jobs is None:
            parser.error("compare requires jobs")
        rows, report = compare(args.directory, json.loads(args.jobs.read_text()))
        (args.directory / "comparison.json").write_text(json.dumps(rows, indent=2) + "\n")
        print(report)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, subprocess.SubprocessError) as error:
        print(f"vendor-cache: {error}", file=sys.stderr)
        raise SystemExit(1) from error
