#!/usr/bin/env python3
"""A deterministic PTY worker fixture: no provider, credentials, or model calls."""
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

if "--version" in sys.argv:
    print("agy protocol fixture")
    sys.exit(0)

sys.stdout.write("\x1b[?2004hagy fixture\n> ")
sys.stdout.flush()
prompt = ""
while "\x1b[201~" not in prompt:
    line = sys.stdin.readline()
    if not line:
        raise RuntimeError("worker prompt ended before bracketed paste closed")
    prompt += line

if "[Krill Worker]" in prompt:
    fields = {
        key: re.search(r"\b" + key + r"=([^\s]+)", prompt).group(1).rstrip(":")
        for key in ("run", "task", "dispatch", "worker")
    }
else:
    fields = {
        key: re.search(label + r":\s*([^\s|]+)", prompt).group(1)
        for key, label in (("run", "Run"), ("task", "Task"), ("dispatch", "Dispatch"), ("worker", "Worker"))
    }

cli = os.environ["KRILL_CLI"]
assert Path(cli).is_absolute()

def invoke(*args):
    return subprocess.check_output([cli, *args], text=True, timeout=20)

guide = invoke("skills", "get", "krill-orchestration", "--reference", "references/worker-contract.md")
assert "Worker obligations" in guide
answer = invoke(
    "orchestration", "ask", "--from", fields["worker"], "--to", "coordinator",
    "--run", fields["run"], "--summary", "한글 질문: 검증을 진행할까요?",
    "--wait", "--timeout-ms", "10000", "--json",
)
assert "진행" in answer, answer
report = Path.cwd() / ("agy-evidence-" + fields["dispatch"] + ".json")
report.write_text(json.dumps({"prompt": prompt, "answer": json.loads(answer), "cli": cli}, ensure_ascii=False))
invoke(
    "orchestration", "send", "--from", fields["worker"], "--to", "coordinator",
    "--type", "worker_done", "--run", fields["run"], "--task", fields["task"],
    "--dispatch", fields["dispatch"], "--outcome", "succeeded",
    "--summary", "질문·응답·근거 보고 완료", "--report-path", str(report), "--json",
)
print("agy-fixture-completion-accepted", flush=True)
time.sleep(60)
