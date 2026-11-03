#!/usr/bin/env python3
"""Run non-Cargo regressions by scope; no product daemon or browser is started."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import contextlib
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import tempfile
import threading


ROOT = Path(__file__).resolve().parent.parent
SCOPES = ("harness", "packaging", "mobile", "telemetry")
# Programs run concurrently; each one's output is buffered and printed whole, in command order.
# They share no fixed paths or ports (each makes its own temp directories and ephemeral ports).
JOBS = 4
ACTIVE = set()
ACTIVE_LOCK = threading.Lock()
STOPPING = threading.Event()
# Explicit inventory of every *.cjs/*.py program in crates/krilld/tests: a standalone Node check (MOBILE),
# a real-Chromium release gate (packaging/release_check.py MOBILE_BROWSER_TESTS), or an entry below saying who runs it.
MOBILE = ("transport", "storage", "terminal", "updates", "quick_commands", "session", "chat", "screens", "diagnostics", "feed")
MOBILE_INTEGRATION = {
    "mobile_pwa.cjs": "compiled holder/daemon; invoked by Rust mobile_pwa integration tests",
    "mobile_install.cjs": "Chrome/real daemon fixture invoked by mobile_pwa.cjs",
    "chat_release_ui.cjs": "helper module loaded by mobile_chat.cjs",
    "mobile_dual_route_browser.py": "manual: needs this Mac on Tailscale with HTTPS and a headed browser",
}


def release_browser_tests():
    sys.path.insert(0, str(ROOT / "packaging"))
    try:
        from release_check import MOBILE_BROWSER_TESTS
    finally:
        sys.path.remove(str(ROOT / "packaging"))
    return {script for script, *_ in MOBILE_BROWSER_TESTS}
PACKAGING_INTEGRATION = {
    "packaging/macos/test_pipeline.py": "native codesign/keychain pipeline; dedicated macOS CI gate",
    "packaging/homebrew/test_release.py": "native DMG/Homebrew integration; dedicated macOS CI gate",
}


def mobile_commands(root):
    directory = root / "crates/krilld/tests"
    standalone = ["mobile_" + name + ".cjs" for name in MOBILE]
    known = set(standalone) | set(MOBILE_INTEGRATION) | release_browser_tests()
    present = {path.name for pattern in ("*.cjs", "*.py") for path in directory.glob(pattern)}
    unknown, missing = sorted(present - known), sorted(known - present)
    if unknown or missing:
        raise ValueError("mobile test inventory: unregistered={}; missing={}. "
                         "Register standalone checks, add browser checks to the release gate, "
                         "or document who runs them."
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


WATCHED = {getattr(signal, name) for name in ("SIGINT", "SIGTERM", "SIGHUP", "SIGALRM") if hasattr(signal, name)}


@contextlib.contextmanager
def deferred_signals():
    # Queue repeated termination signals until the bounded cleanup finishes, then re-deliver
    # them, so cancellation is deferred, not lost. A thread mask would not do: with worker
    # threads the kernel may hand the signal to a worker and Python still runs it here.
    if threading.current_thread() is not threading.main_thread():
        yield
        return
    pending = []
    previous = {sig: signal.signal(sig, lambda signum, _frame: pending.append(signum)) for sig in WATCHED}
    try:
        yield
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)
        for sig in pending[:1]:
            signal.raise_signal(sig)


def stop(child):
    with deferred_signals():
        stop_group(child)


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


def run(command, root, timeout, output=None):
    print("regression: " + shlex.join(command), file=output or sys.stdout, flush=True)
    # Popen's context manager waits without a deadline on exit, even if cleanup
    # cannot signal a child. Keep every wait bounded and report the failure.
    with ACTIVE_LOCK:
        if STOPPING.is_set():
            return 130
        child = subprocess.Popen(command, cwd=root, start_new_session=(os.name == "posix"),
                                 stdout=output, stderr=output and subprocess.STDOUT)
        ACTIVE.add(child)
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
    finally:
        with ACTIVE_LOCK:
            ACTIVE.discard(child)


def run_buffered(command, root, timeout):
    with tempfile.TemporaryFile("w+") as output:
        try:
            result = run(command, root, timeout, output)
        except OSError as error:
            output.write("regression: could not execute {}: {}\n".format(shlex.join(command), error))
            result = 1
        output.seek(0)
        return result, output.read()


def run_all(checks, root, timeout):
    """Run concurrently; print each program's output in order. Returns the failed commands."""
    failed = []
    with ThreadPoolExecutor(max_workers=JOBS) as pool:
        futures = [pool.submit(run_buffered, command, root, timeout) for command in checks]
        try:
            for command, future in zip(checks, futures):
                result, text = future.result()
                sys.stdout.write(text)
                sys.stdout.flush()
                if result != 0:
                    failed.append(shlex.join(command))
        except BaseException:
            # Signals arrive in this thread; stop every running program group before exiting.
            with deferred_signals():
                STOPPING.set()
                for future in futures:
                    future.cancel()
                with ACTIVE_LOCK:
                    children = list(ACTIVE)
                for child in children:
                    stop(child)
            raise
    return failed


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
        if args.list:
            for command in checks:
                print(shlex.join(command))
            return 0
        failed = run_all(checks, ROOT, args.timeout)
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
