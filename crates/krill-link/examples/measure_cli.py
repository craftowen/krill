#!/usr/bin/env python3
"""Measure actual release CLI processes; pause the owned fixture server to force idle timeout.
Usage: python3 crates/krill-link/examples/measure_cli.py RELEASE_DIR OUTPUT_DIR
"""
import json
import os
import pathlib
import re
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time

binary = pathlib.Path(sys.argv[1]).resolve() / "krill-link"
output = pathlib.Path(sys.argv[2]).resolve()
output.mkdir(parents=True, exist_ok=True)
base = pathlib.Path(tempfile.mkdtemp(prefix="m134-cli-", dir="/tmp"))
server_dir, client_dir = base / "s", base / "c"
(server_dir / "run").mkdir(parents=True)
client_dir.mkdir()
processes, streams, samples = [], [], []
server = None


def command(directory, *args):
    return subprocess.check_output([str(binary), *args, "--state-dir", str(directory)], text=True)


def snapshot():
    return json.loads(command(client_dir, "status", "--json"))["hosts"][0]


def sample(phase):
    record = {"phase": phase, "status": snapshot()}
    for role, process in zip(("serve", "connect"), processes):
        text = subprocess.check_output(["/usr/bin/footprint", "-f", "bytes", "--noCategories", str(process.pid)], text=True)
        (output / f"{phase}-{role}.txt").write_text(text)
        for label, field in (("phys_footprint", "bytes"), ("phys_footprint_peak", "peak_bytes")):
            record[f"{role}_{field}"] = int(re.search(rf"{label}:\s*([\d,]+)", text)[1].replace(",", ""))
    samples.append(record)
    print(json.dumps(record), flush=True)


def echo(connection):
    try:
        with connection:
            while data := connection.recv(8192):
                connection.sendall(data)
    except (BrokenPipeError, ConnectionResetError):
        pass


def accept_loop(listener):
    while True:
        try:
            connection, _ = listener.accept()
            threading.Thread(target=echo, args=(connection,), daemon=True).start()
        except OSError:
            return


try:
    for directory in (server_dir, client_dir):
        command(directory, "config", "--relay", "off", "--server-name", "resume-bench", "--replay-buffer-bytes", "4194304")
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(server_dir / "run/krilld.sock"))
    listener.listen(8)
    threading.Thread(target=accept_loop, args=(listener,), daemon=True).start()
    server = subprocess.Popen([str(binary), "serve", "--state-dir", str(server_dir)], stdout=subprocess.DEVNULL)
    processes.append(server)
    deadline = time.monotonic() + 10
    while not (server_dir / "run/link-ctl.sock").exists():
        assert time.monotonic() < deadline
        time.sleep(0.01)
    invite = json.loads(command(server_dir, "invite", "--json", "--no-qr"))
    host = json.loads(command(client_dir, "pair", invite["link"], "--name", "cli probe", "--json"))
    client = subprocess.Popen([str(binary), "connect", host["id"], "--state-dir", str(client_dir)], stdout=subprocess.DEVNULL)
    processes.append(client)
    path = client_dir / f"run/hosts/{host['id']}.sock"
    while not path.exists():
        assert time.monotonic() < deadline
        time.sleep(0.01)
    for _ in range(4):
        stream = socket.socket(socket.AF_UNIX)
        stream.settimeout(15)
        stream.connect(str(path))
        stream.sendall(b"warm")
        assert stream.recv(4) == b"warm"
        streams.append(stream)
    while len(snapshot()["streams"]) != 4 or not all(s["state"] == "normal" for s in snapshot()["streams"]):
        assert time.monotonic() < deadline
        time.sleep(0.02)
    sample("idle")
    os.kill(server.pid, signal.SIGSTOP)
    while len(snapshot()["streams"]) != 4 or not all(s["state"] == "suspended" for s in snapshot()["streams"]):
        assert time.monotonic() < deadline + 10
        time.sleep(0.05)
    payload = (bytes(range(251)) * ((960 * 1024 // 251) + 1))[:960 * 1024]
    for stream in streams:
        stream.sendall(payload)
    deadline = time.monotonic() + 10
    while sum(s["buffered_bytes"] for s in snapshot()["streams"]) < len(payload) * 4:
        assert time.monotonic() < deadline
        time.sleep(0.05)
    sample("saturated")
    try:
        streams[0].sendall(b"x" * (512 * 1024))
    except (BrokenPipeError, ConnectionResetError):
        pass
    while len(snapshot()["streams"]) != 3:
        assert time.monotonic() < deadline
        time.sleep(0.02)
    sample("overflow_isolated")
    started = time.monotonic()
    os.kill(server.pid, signal.SIGCONT)
    for stream in streams[1:]:
        received = bytearray()
        while len(received) < len(payload):
            chunk = stream.recv(len(payload) - len(received))
            assert chunk, "EOF while draining resumed output"
            received.extend(chunk)
        assert received == payload
    while not all(s["state"] == "normal" and s["buffered_bytes"] == 0 for s in snapshot()["streams"]):
        assert time.monotonic() < started + 15
        time.sleep(0.02)
    elapsed_ms = (time.monotonic() - started) * 1000
    sample("resumed")
    samples[-1]["server_continue_to_complete_ms"] = elapsed_ms
finally:
    if server is not None and server.poll() is None:
        os.kill(server.pid, signal.SIGCONT)
    for stream in streams:
        stream.close()
    for process in reversed(processes):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
    (output / "measurements.json").write_text(json.dumps(samples, indent=2) + "\n")
    import shutil
    shutil.rmtree(base)
assert samples and all(s[f"{role}_peak_bytes"] <= 20_000_000 for s in samples for role in ("serve", "connect"))
