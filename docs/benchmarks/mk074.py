#!/usr/bin/env python3
"""macOS Krill GUI benchmark and read-only running Orca memory snapshot.

Startup ends at the first on-screen layer-0 window >=100x100, not first paint
or terminal readiness. Orca is NEVER launched or signaled. Requires a logged-in GUI.
"""

import argparse
import ctypes as C
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import signal
import statistics
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]


class Rusage(C.Structure):
    # sys/resource.h rusage_info_v0 (SDK); request flavor 0, no RSS substitution.
    _fields_ = [("uuid", C.c_ubyte * 16)] + [(name, C.c_uint64) for name in (
        "user", "system", "idle_wakeups", "interrupt_wakeups", "pageins",
        "wired", "resident", "footprint", "start", "exit")]


def usage(pid):
    lib = C.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    lib.proc_pid_rusage.argtypes = [C.c_int, C.c_int, C.c_void_p]
    info = Rusage()
    if lib.proc_pid_rusage(pid, 0, C.byref(info)):
        raise OSError(C.get_errno(), f"proc_pid_rusage({pid})")
    return info


def windows():
    cg = C.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
    cf = C.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
    cg.CGWindowListCopyWindowInfo.argtypes = [C.c_uint32, C.c_uint32]
    cg.CGWindowListCopyWindowInfo.restype = C.c_void_p
    cf.CFPropertyListCreateData.argtypes = [C.c_void_p, C.c_void_p, C.c_long, C.c_ulong, C.c_void_p]
    cf.CFPropertyListCreateData.restype = C.c_void_p
    cf.CFDataGetLength.argtypes = [C.c_void_p]
    cf.CFDataGetLength.restype = C.c_long
    cf.CFDataGetBytePtr.argtypes = [C.c_void_p]
    cf.CFDataGetBytePtr.restype = C.c_void_p
    cf.CFRelease.argtypes = [C.c_void_p]
    array = cg.CGWindowListCopyWindowInfo(1, 0)  # on-screen metadata only
    if not array:
        raise RuntimeError("WindowServer unavailable")
    data = cf.CFPropertyListCreateData(None, array, 200, 0, None)
    try:
        if not data:
            raise RuntimeError("cannot serialize window metadata")
        return plistlib.loads(C.string_at(cf.CFDataGetBytePtr(data), cf.CFDataGetLength(data)))
    finally:
        if data:
            cf.CFRelease(data)
        cf.CFRelease(array)


def has_window(pid, entries):
    return any(w.get("kCGWindowOwnerPID") == pid and w.get("kCGWindowLayer") == 0
               and w.get("kCGWindowBounds", {}).get("Width", 0) >= 100
               and w.get("kCGWindowBounds", {}).get("Height", 0) >= 100 for w in entries)


def processes():
    out = subprocess.check_output(["/bin/ps", "-axo", "pid=,ppid=,comm="], text=True)
    return {int(p): (int(parent), command) for line in out.splitlines()
            for p, parent, command in [line.strip().split(None, 2)]}


def descendants(root, table):
    found = {root}
    while True:
        more = {p for p, (parent, _) in table.items() if parent in found} - found
        if not more:
            return found
        found.update(more)


def remember(root, owned):
    table = processes()
    try:
        if root in owned and usage(root).start != owned[root]:
            return table
    except OSError:
        return table
    def depth(pid):
        seen = set()
        while pid in table and pid != root and pid not in seen:
            seen.add(pid)
            pid = table[pid][0]
        return len(seen)
    for pid in sorted(descendants(root, table), key=depth):
        if pid not in owned:
            try:
                owned[pid] = usage(pid).start
            except OSError:
                pass  # process exited during enumeration
    return table


def cleanup(owned):
    # Stop parents first (holder must not respawn krilld), then their children.
    for pid, start in owned.items():
        try:
            if usage(pid).start == start:
                os.kill(pid, signal.SIGKILL)
        except (OSError, ProcessLookupError):
            pass


def isolated_env(folder):
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("ORCA_", "KRILL_", "MK_", "ELECTRON_"))}
    env.update(HOME=str(folder), XDG_CONFIG_HOME=str(folder / "config"),
               XDG_CACHE_HOME=str(folder / "cache"), TMPDIR=str(folder))
    return env


def trial(executable, settle):
    owned = {}
    with tempfile.TemporaryDirectory(prefix="mk074-", dir="/tmp") as temp:
        folder = Path(temp)
        env = isolated_env(folder)
        env["KRILL_STATE_DIR"] = str(folder / "state")
        env["MK_STATE_DIR"] = str(folder / "state")
        env["SHELL"] = "/bin/sh"  # no user login-shell setup
        with (folder / "app.log").open("w") as log:
            start = time.monotonic_ns()
            child = subprocess.Popen([str(executable)], env=env, cwd=folder, stdin=subprocess.DEVNULL,
                                     stdout=log, stderr=log)
            try:
                owned[child.pid] = usage(child.pid).start
                deadline = time.monotonic() + 30
                while True:
                    if child.poll() is not None:
                        raise RuntimeError(f"Krill exited ({child.returncode}); log: "
                                           + (folder / "app.log").read_text()[-3000:])
                    remember(child.pid, owned)
                    if has_window(child.pid, windows()):
                        break
                    if time.monotonic() > deadline:
                        log.flush()
                        raise TimeoutError("Krill: no on-screen window in 30s; log: "
                                           + (folder / "app.log").read_text()[-5000:])
                    time.sleep(0.02)
                launch_ms = (time.monotonic_ns() - start) / 1e6
                deadline = time.monotonic() + settle
                while time.monotonic() < deadline:
                    remember(child.pid, owned)
                    time.sleep(0.1)
                if child.poll() is not None or not has_window(child.pid, windows()):
                    raise RuntimeError("Krill exited or lost its window during settle")
                table = remember(child.pid, owned)
                samples = []
                for pid, identity in owned.items():
                    try:
                        info = usage(pid)
                    except OSError:
                        continue
                    if info.start != identity:
                        continue
                    command = table.get(pid, (0, "exited"))[1]
                    # Compare app/runtime overhead, excluding terminal shell/workload.
                    if Path(command).name not in ("krill-app", "krill-holder", "mk-app", "mk-holder", "krilld"):
                        continue
                    samples.append({"pid": pid, "process": Path(command).name,
                                    "phys_footprint_bytes": info.footprint})
                return {"window_ms": launch_ms, "processes": samples,
                        "phys_footprint_bytes": sum(p["phys_footprint_bytes"] for p in samples)}
            finally:
                try:
                    remember(child.pid, owned)
                finally:
                    cleanup(owned)
                    child.wait(timeout=5)


def orca_snapshot():
    # Read only. No Orca CLI/runtime mutation, launch, activation, or signal.
    out = subprocess.check_output(["/bin/ps", "-axo", "pid=,ppid=,rss=,comm="], text=True)
    args = subprocess.check_output(["/bin/ps", "-axo", "pid=,args="], text=True)
    cli_pids = {int(line.split(None, 1)[0]) for line in args.splitlines()
                if "/out/cli/" in line or "/out/cli/index.js" in line}
    samples, skipped = [], []
    for line in out.splitlines():
        pid, parent, rss, command = line.strip().split(None, 3)
        pid = int(pid)
        if pid in cli_pids or not (command.startswith("/Applications/Orca.app/")
                                  or "/orca/daemon-host/" in command):
            continue
        result = subprocess.run(["/usr/bin/footprint", "-p", str(pid), "-f", "bytes", "--noCategories"],
                                capture_output=True, text=True, timeout=15, check=False)
        match = re.search(r"phys_footprint:\s*(\d+) B", result.stdout)
        if result.returncode or not match:
            skipped.append(pid)
            continue
        samples.append({"pid": pid, "ppid": int(parent), "process": Path(command).name,
                        "rss_bytes": int(rss) * 1024, "phys_footprint_bytes": int(match[1])})
    if not samples:
        raise RuntimeError("no running Orca process could be measured")
    return {"timestamp_utc": datetime.now(timezone.utc).isoformat(), "processes": samples,
            "skipped_pids": skipped, "rss_bytes": sum(p["rss_bytes"] for p in samples),
            "phys_footprint_bytes": sum(p["phys_footprint_bytes"] for p in samples)}


def orca_version():
    try:
        info = plistlib.loads(Path("/Applications/Orca.app/Contents/Info.plist").read_bytes())
    except OSError:
        return "unknown (Orca.app Info.plist unreadable)"
    return info.get("CFBundleShortVersionString", "unknown")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--krill", "--minikrill", dest="krill", type=Path,
                        default=ROOT / "target/release/krill-app")
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--settle", type=float, default=5)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.rounds < 1 or args.settle < 0:
        parser.error("rounds >= 1 and settle >= 0 required")
    if args.krill.name not in ("krill-app", "mk-app") or not args.krill.is_file():
        parser.error("--krill must name an existing krill-app binary")
    report = {"timestamp_utc": datetime.now(timezone.utc).isoformat(),
              "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
              "macos": subprocess.check_output(["sw_vers"], text=True).strip(),
              "hardware": subprocess.check_output(["sysctl", "-n", "hw.model", "hw.memsize", "machdep.cpu.brand_string"], text=True).strip(),
              "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
              "orca_version": orca_version(),
              "method": "fresh isolated profile, first on-screen >=100x100 layer-0 window; not first paint or session readiness",
              "workload": "Krill fresh first-run window/default session, no agents; Orca existing live IDE/worker workload; not an equal-workload comparison",
              "memory_method": "sum per-PID phys_footprint (may double-count shared memory); Orca footprint(1) and ps RSS; excludes agent/shell processes and Orca CLI invocations",
              "settle_seconds": args.settle, "results": {}}
    binary = args.krill.resolve()
    trials = [trial(binary, args.settle) for _ in range(args.rounds)]
    holder_name = "krill-holder" if (binary.parent / "krill-holder").is_file() else "mk-holder"
    report["results"]["krill"] = {
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "helper_sha256": {name: hashlib.sha256((binary.parent / name).read_bytes()).hexdigest()
                          for name in (holder_name, "krilld")},
        "trials": trials,
        "median_window_ms": statistics.median(t["window_ms"] for t in trials),
        "median_phys_footprint_bytes": statistics.median(t["phys_footprint_bytes"] for p in trials for t in [p])}
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    snapshots = [orca_snapshot() for _ in range(args.rounds)]
    report["results"]["orca"] = {
        "startup": "미확인; Orca was not launched or restarted",
        "snapshots": snapshots,
        "median_phys_footprint_bytes": statistics.median(t["phys_footprint_bytes"] for t in snapshots),
        "median_rss_bytes": statistics.median(t["rss_bytes"] for t in snapshots)}
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({k: {field: value for field, value in v.items() if field.startswith("median")}
                      for k, v in report["results"].items()}, indent=2))


if __name__ == "__main__":
    main()
