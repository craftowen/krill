#!/usr/bin/env python3
"""Measure the release probe's connect process and actual krill-link serve process.
Usage: python3 crates/krill-link/examples/measure_resume.py /path/to/target/release OUTPUT_DIR
The probe waits at each phase until footprint has measured both processes.
"""
import json
import pathlib
import re
import subprocess
import sys

release = pathlib.Path(sys.argv[1]).resolve()
output = pathlib.Path(sys.argv[2]).resolve()
output.mkdir(parents=True, exist_ok=True)
records = []
probe = subprocess.Popen(
    [str(release / "examples/resume_probe")],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
)
try:
    for line in probe.stdout:
        record = json.loads(line)
        for role in ("connect", "serve"):
            result = subprocess.run(
                ["/usr/bin/footprint", "-f", "bytes", "--noCategories", str(record[f"{role}_pid"])],
                capture_output=True, text=True, check=True,
            )
            raw = result.stdout + result.stderr
            (output / f"{record['phase']}-{role}.txt").write_text(raw)
            match = re.search(r"phys_footprint:\s*([\d,]+)", raw, re.IGNORECASE)
            if not match:
                raise RuntimeError(f"Cannot parse footprint: {raw}")
            record[f"{role}_footprint_bytes"] = int(match.group(1).replace(",", ""))
            peak = re.search(r"phys_footprint_peak:\s*([\d,]+)", raw)
            record[f"{role}_peak_bytes"] = int(peak.group(1).replace(",", ""))
        records.append(record)
        print(json.dumps(record), flush=True)
        probe.stdin.write("continue\n")
        probe.stdin.flush()
    errors = probe.stderr.read()
    code = probe.wait(timeout=20)
    if code:
        raise RuntimeError(f"Probe exited {code}: {errors}")
finally:
    if probe.poll() is None:
        probe.kill()
        probe.wait()
    (output / "measurements.json").write_text(json.dumps(records, indent=2) + "\n")
if not records or any(
    record[f"{role}_peak_bytes"] > 20_000_000
    for record in records for role in ("connect", "serve")
):
    raise SystemExit("20,000,000 byte physical footprint gate failed")
