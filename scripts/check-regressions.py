#!/usr/bin/env python3
"""Run non-Cargo regressions by scope; no product daemon or browser is started."""

import argparse
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
SCOPES = ("harness", "packaging", "mobile", "telemetry")
# Explicit inventory: mobile_pwa.cjs needs compiled helpers; *_browser.* needs Chrome.
# Those remain integration/visual checks, not standalone Node test programs.
MOBILE = ("transport", "storage", "terminal", "updates", "quick_commands", "session", "chat", "screens", "diagnostics", "feed")
MOBILE_INTEGRATION = {
    "mobile_pwa.cjs": "compiled holder/daemon; invoked by Rust mobile_pwa integration tests",
    "mobile_install.cjs": "Chrome/real daemon fixture invoked by mobile_pwa.cjs",
    "mobile_scroll_browser.cjs": "real Chromium touch/scroll integration",
    "mobile_terminal_scroll_browser.cjs": "real Chromium terminal/CSS integration",
}
PACKAGING_INTEGRATION = {
    "packaging/macos/test_pipeline.py": "native codesign/keychain pipeline; dedicated macOS CI gate",
    "packaging/homebrew/test_release.py": "native DMG/Homebrew integration; dedicated macOS CI gate",
}


def mobile_commands(root):
    directory = root / "crates/krilld/tests"
    standalone = ["mobile_" + name + ".cjs" for name in MOBILE]
    known = set(standalone) | set(MOBILE_INTEGRATION)
    present = {path.name for path in directory.glob("mobile_*.cjs")}
    unknown, missing = sorted(present - known), sorted(known - present)
    if unknown or missing:
        raise ValueError("mobile test inventory: unregistered={}; missing={}. "
                         "Register standalone checks or document their integration dependency."
                         .format(unknown, missing))
    return [["node", str(directory / name)] for name in standalone]


def commands(root, scopes):
    checks = []
    for scope in dict.fromkeys(scopes):
        if scope == "harness":
            files = sorted(root.glob("scripts/test_*.py"))
        elif scope == "packaging":
            files = sorted(path for path in root.glob("packaging/**/test_*.py")
                           if path.relative_to(root).as_posix() not in PACKAGING_INTEGRATION)
            if not files:
                raise ValueError("no packaging test files found")
            files += sorted(root.glob("docs/licensing/test_*.py"))
        elif scope == "mobile":
            checks += mobile_commands(root)
            continue
        else:
            files = sorted(root.glob("services/telemetry/test/*.test.js"))
            if not files:
                raise ValueError("no telemetry test files found")
            checks.append(["node", "--test", *map(str, files)])
            continue
        if not files:
            raise ValueError("no test files found for " + scope)
        checks += [[sys.executable, str(path)] for path in files]
    return checks


def stop(child):
    # Queue repeated termination signals until the bounded cleanup finishes.
    # Unmasking delivers them afterward, so cancellation is deferred, not lost.
    previous_mask = None
    if hasattr(signal, "pthread_sigmask"):
        watched = {getattr(signal, name) for name in ("SIGINT", "SIGTERM", "SIGHUP", "SIGALRM")
                   if hasattr(signal, name)}
        previous_mask = signal.pthread_sigmask(signal.SIG_BLOCK, watched)
    try:
        stop_group(child)
    finally:
        if previous_mask is not None:
            signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)


def stop_group(child):
    # Own process group only; timeout must not leave grandchildren holding build locks.
    def terminate(sig):
        try:
            if os.name == "posix":
                os.killpg(child.pid, sig)
            else:
                child.kill()
        except ProcessLookupError:
            pass
        except PermissionError as error:
            # Cleanup is called only on failure/interruption paths. A denied signal
            # must remain a reported failure, rather than hiding the original result.
            print("regression: cannot signal process group {}: {}".format(child.pid, error),
                  file=sys.stderr)
    terminate(signal.SIGTERM)
    try:
        child.wait(timeout=3)
    except subprocess.TimeoutExpired:
        pass
    # The parent may have exited while descendants are still running.
    terminate(getattr(signal, "SIGKILL", signal.SIGTERM))
    try:
        child.wait(timeout=3)
    except subprocess.TimeoutExpired:
        print("regression: could not reap process {} after cleanup".format(child.pid), file=sys.stderr)


def run(command, root, timeout):
    print("regression: " + shlex.join(command), flush=True)
    # Popen's context manager waits without a deadline on exit, even if cleanup
    # cannot signal a child. Keep every wait bounded and report the failure.
    child = subprocess.Popen(command, cwd=root, start_new_session=(os.name == "posix"))
    try:
        result = child.wait(timeout=timeout)
        if os.name == "posix":
            try:
                os.killpg(child.pid, 0)
            except ProcessLookupError:
                pass
            else:
                stop(child)
                print("regression: program exited with leftover descendants", file=sys.stderr)
                return result or 1
        return result
    except subprocess.TimeoutExpired:
        stop(child)
        print("regression: timed out", file=sys.stderr)
        return 124
    except BaseException:
        stop(child)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scope", action="append", choices=SCOPES,
                        help="repeat to select scopes; default: all")
    parser.add_argument("--list", action="store_true", help="print commands without running them")
    parser.add_argument("--timeout", type=int, default=180, help="seconds per test program (default: 180)")
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    try:
        checks = commands(ROOT, args.scope or SCOPES)
        failed = []
        for command in checks:
            if args.list:
                print(shlex.join(command))
            else:
                try:
                    failed_check = run(command, ROOT, args.timeout) != 0
                except OSError as error:
                    print("regression: could not execute {}: {}".format(shlex.join(command), error),
                          file=sys.stderr)
                    failed_check = True
                if failed_check:
                    failed.append(shlex.join(command))
        if not args.list:
            print("regression: {} programs, {} failed".format(len(checks), len(failed)), flush=True)
            for command in failed:
                print("FAILED: " + command, file=sys.stderr)
        return int(bool(failed))
    except (OSError, ValueError) as error:
        print("regression: " + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    for name in ("SIGTERM", "SIGHUP", "SIGALRM"):
        sig = getattr(signal, name, None)
        if sig is not None:
            signal.signal(sig, lambda signum, _frame: sys.exit(128 + signum))
    sys.exit(main())
