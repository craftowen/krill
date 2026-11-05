#!/usr/bin/env python3
"""최종 서명 번들의 CLI·GUI 실행과 앱/데몬 SIGKILL 뒤 PTY 생존을 격리 경로에서 확인한다."""
import contextlib
import json
from pathlib import Path
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time


def wait_for(label, condition, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = condition()
        if result:
            return result
        time.sleep(0.1)
    raise RuntimeError(f"signed bundle smoke timed out: {label}")


def kill_home_processes(root):
    """Kill every process still running with HOME set to the temporary root.

    The daemon probes real agent CLIs, and opencode detaches a managed service that
    leaves the process tree and keeps writing into HOME after cleanup starts.
    """
    marker, quiet = f"HOME={root}", 0
    while quiet < 2:
        listing = subprocess.run(["/bin/ps", "axeww", "-o", "pid=,command="],
                                 capture_output=True, text=True, timeout=10).stdout
        pids = [int(line.split(None, 1)[0]) for line in listing.splitlines() if marker in line.split()]
        for pid in pids:
            with contextlib.suppress(ProcessLookupError):
                os.kill(pid, signal.SIGKILL)
        quiet = 0 if pids else quiet + 1
        time.sleep(0.5)


def remove_home(root):
    """Delete the temporary HOME, retrying while late agent processes still write into it."""
    for _ in range(20):
        kill_home_processes(root)
        try:
            shutil.rmtree(root)
            return
        except OSError:
            if not root.exists():
                return
            time.sleep(0.5)
    shutil.rmtree(root)


def smoke(bundle):
    bundle = Path(bundle).resolve()
    temporary = tempfile.mkdtemp(prefix="krill-signed-", dir="/tmp")
    try:
        root = Path(temporary)
        env = dict(
            os.environ,
            HOME=str(root),
            KRILL_STATE_DIR=str(root),
            KRILL_NO_UPDATE_CHECK="1",
            KRILL_TELEMETRY_DISABLED="1",
            KRILL_ERROR_REPORTING_DISABLED="1",
        )
        holder = None
        apps = []
        session = None
        session_pid = None
        session_pids = set()
        daemons = set()
        cli_path = bundle / "Contents/MacOS/krill"

        def cli(*args):
            result = subprocess.run([str(cli_path), *map(str, args)], cwd=root, env=env,
                                    capture_output=True, text=True, check=True, timeout=30)
            return result.stdout.strip()

        def daemon_pid():
            result = subprocess.run(["/usr/bin/pgrep", "-P", str(holder.pid), "-x", "krilld"],
                                    capture_output=True, text=True, timeout=5)
            pids = result.stdout.split()
            if len(pids) == 1:
                pid = int(pids[0])
                daemons.add(pid)
                return pid
            return None

        def launch_app(log):
            app = subprocess.Popen([str(bundle / "Contents/MacOS/krill-app")], cwd=root, env=env,
                stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
            apps.append(app)

            def attached():
                if app.poll() is not None:
                    raise RuntimeError("signed GUI exited before attaching")
                result = subprocess.run(["/usr/sbin/lsof", "-a", "-p", str(app.pid), "-U", "-Ffn"],
                                        capture_output=True, text=True, timeout=5)
                endpoints = [line for line in result.stdout.splitlines() if line.startswith("n")]
                return any(endpoints.count(endpoint) >= 3 for endpoint in endpoints)
            wait_for("signed GUI attachment", attached)
            return app

        def exchange(marker):
            cli("send", session, marker)
            cli("send", session, "--enter")
            cli("wait", session, "--for", "pattern", "--pattern", "signed:" + marker, "--timeout", "10")

        try:
            with (root / "smoke.log").open("w+") as log:
                holder = subprocess.Popen([str(bundle / "Contents/Helpers/krill-holder"),
                    str(bundle / "Contents/Helpers/krilld"), str(root / "run/krilld.sock")], cwd=root, env=env,
                    stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
                wait_for("signed daemon socket", lambda: (root / "run/krilld.sock").is_socket())
                wait_for("signed daemon PID", daemon_pid)
                created = json.loads(cli("new", "--json", "--cwd", root, "--", "/bin/sh", "-c",
                    'stty -echo; while IFS= read -r line; do printf "signed:%s\\n" "$line"; done'))
                session, session_pid = created["session"], created["pid"]
                session_pids.add(session_pid)
                exchange("before")
                app = launch_app(log)
                app.kill()
                app.wait(timeout=5)
                exchange("after-app-kill")
                launch_app(log)
                old = wait_for("daemon PID before kill", daemon_pid)
                os.kill(old, signal.SIGKILL)
                wait_for("signed daemon restart", lambda: (pid := daemon_pid()) and pid != old)
                exchange("after-daemon-kill")
                print("signed bundle smoke PASS: CLI/PTY, GUI attach/relaunch, app SIGKILL, daemon SIGKILL survival", flush=True)
        finally:
            with contextlib.suppress(Exception):
                session_pids.update(row["pid"] for row in json.loads(cli("ls", "--json")))
            if session is not None:
                with contextlib.suppress(Exception):
                    cli("kill", session)
            for child in apps:
                if child.poll() is None:
                    child.kill()
                child.wait(timeout=5)
            if holder:
                with contextlib.suppress(Exception):
                    daemon_pid()
                if holder.poll() is None:
                    holder.kill()
                holder.wait(timeout=5)
            # 이 테스트의 holder 자식·직접 생성한 PTY 세션 PID만 정리한다.
            for pid in daemons | session_pids:
                with contextlib.suppress(ProcessLookupError):
                    os.kill(pid, signal.SIGKILL)
    finally:
        remove_home(Path(temporary))


if __name__ == "__main__":
    smoke(Path(sys.argv[1]))
