#!/usr/bin/python3
"""Offline app-server fixture: real file edits, version conflicts, exact RPC shapes.

This does not execute a model or touch auth. TOML support is deliberately confined
to fixture-generated trust tables; production krill parses TOML using the Rust crate.
"""
import hashlib
import json
import os
import re
import sys

path = os.path.join(os.environ["CODEX_HOME"], "config.toml")


def text():
    try:
        with open(path) as f:
            return f.read()
    except FileNotFoundError:
        return ""


def version():
    return hashlib.sha256(text().encode()).hexdigest()


events = []
command = None
for arg in sys.argv:
    match = re.fullmatch(r'hooks\.(\w+)=\[\{hooks=\[\{type="command",command=(.+)\}\]\}\]', arg)
    if match:
        events.append(match.group(1))
        command = json.loads(match.group(2))

for line in sys.stdin:
    req = json.loads(line)
    if "id" not in req:
        continue
    method = req["method"]
    if method == "initialize":
        result = {}
    elif method == "config/read":
        result = {"layers": [{"name": {"type": "user", "file": path}, "version": version()}]}
    elif method == "hooks/list":
        result = {"data": [{"hooks": [{"command": command,
                  "key": "/<session-flags>/config.toml:" + re.sub(r'(?<!^)(?=[A-Z])', '_', e).lower() + ":0:0",
                  "currentHash": "sha256:" + hashlib.sha256((e + command).encode()).hexdigest(),
                  "trustStatus": "untrusted"} for e in events]}]}
    elif method == "config/batchWrite":
        if os.environ.get("MK413_FAKE_FAIL_WRITE"):
            print(json.dumps({"id": req["id"], "error": {"message": "version conflict"}}), flush=True)
            continue
        params = req["params"]
        assert params["filePath"] == path
        if params["expectedVersion"] != version():
            print(json.dumps({"id": req["id"], "error": {"message": "version conflict"}}), flush=True)
            continue
        edits = params["edits"]
        assert len(edits) == 1 and edits[0]["keyPath"] == "hooks.state" and edits[0]["mergeStrategy"] == "replace"
        old = re.sub(r'\n?\[hooks\.state\."[^"\n]+"\]\n(?:[^\[\n].*\n)*', '', text())
        new = old.rstrip() + "\n"
        for key, value in edits[0]["value"].items():
            new += '\n[hooks.state.' + json.dumps(key) + ']\n'
            for field, val in value.items():
                new += field + ' = ' + json.dumps(val) + '\n'
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path + ".tmp", "w") as f:
            f.write(new)
        os.replace(path + ".tmp", path)
        result = {"filePath": path}
    else:
        raise AssertionError(method)
    print(json.dumps({"id": req["id"], "result": result}), flush=True)
