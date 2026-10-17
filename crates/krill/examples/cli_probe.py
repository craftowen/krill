#!/usr/bin/env python3
"""MK-045 macOS peak RSS probe. Builds must already exist; no application is launched.

python3 crates/krill/examples/cli_probe.py target/release/krill
All daemons and sessions belong to the probe's temporary KRILL_STATE_DIR.
"""
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import fcntl

krill = Path(sys.argv[1]).resolve()
measurements = {}
with tempfile.TemporaryDirectory(prefix="mk045-", dir="/tmp") as state:
    env = {**os.environ, "KRILL_STATE_DIR": state}
    holder = subprocess.Popen([str(krill.with_name("krill-holder")), str(krill.with_name("krilld")), state + "/run/krilld.sock"], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    daemon = None
    sessions = []

    def run(*args, measured=False):
        command = (["/usr/bin/time", "-l"] if measured else []) + [str(krill), *args]
        p = subprocess.run(command, env=env, capture_output=True, timeout=15)
        if p.returncode:
            raise RuntimeError((args, p.returncode, p.stderr.decode()))
        if measured:
            record(args[0], p.stderr)
        return p.stdout.decode().strip()

    def record(name, stderr):
        rss = int(re.search(rb"(\d+)\s+maximum resident set size", stderr)[1])
        measurements.setdefault(name, []).append(rss)
        if rss >= 10_000_000:
            raise AssertionError(f"{name}: peak RSS {rss} >= 10MB")

    def until_read(session, text):
        deadline = time.monotonic() + 10
        while text not in run("read", session):
            if time.monotonic() > deadline:
                raise TimeoutError(text)
            time.sleep(.02)

    try:
        deadline = time.monotonic() + 10
        while not Path(state + "/run/krilld.sock").exists():
            if time.monotonic() > deadline:
                raise TimeoutError("daemon startup")
            time.sleep(.02)
        doctor = run("doctor", measured=True)
        daemon = int(re.search(r"instance (\d+)", doctor)[1])
        for iteration in range(3):
            session = run("new", "--", "/bin/sh", "-c", "stty -echo; printf 'ready\\n'; IFS= read -r line; printf 'probe:%s\\n' \"$line\"; IFS= read -r line", measured=True)
            sessions.append(session)
            until_read(session, "ready")
            run("ls", measured=True)
            run("read", session, measured=True)
            run("wait", session, "--for", "unknown", "--timeout", "1", measured=True)
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
            with tempfile.TemporaryFile() as stderr:
                p = subprocess.Popen(["/usr/bin/time", "-l", str(krill), "attach", session], env=env, stdin=slave, stdout=slave, stderr=stderr)
                os.close(slave)
                data = bytearray()

                def receive(text):
                    deadline = time.monotonic() + 10
                    while text not in data:
                        if time.monotonic() > deadline:
                            raise TimeoutError((text, data))
                        if select.select([master], [], [], .1)[0]:
                            data.extend(os.read(master, 65536))

                try:
                    receive(b"ready")
                    os.write(master, b"attached\r")
                    receive(b"probe:attached")
                    os.write(master, b"\x1d")
                    assert p.wait(timeout=10) == 0
                    stderr.seek(0)
                    record("attach", stderr.read())
                finally:
                    if p.poll() is None:
                        p.kill()
                        p.wait()
                    os.close(master)
            run("send", session, "--enter", "finish", measured=True)
            run("wait", session, "--timeout", "5", measured=True)
        run("doctor", measured=True)
        print(json.dumps({"peak_rss_bytes": measurements, "maximum_bytes": max(max(v) for v in measurements.values())}, indent=2))
    finally:
        # Every shell normally exits after 'finish'. On failures, terminate only our recorded roots.
        try:
            listing = run("ls")
            for row in listing.splitlines()[1:]:
                pid = int(row.split()[1])
                try:
                    os.killpg(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
        finally:
            holder.kill()
            holder.wait()
            if daemon is not None:
                try:
                    os.kill(daemon, signal.SIGKILL)
                except ProcessLookupError:
                    pass
