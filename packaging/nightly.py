#!/usr/bin/env python3
"""자정에 고정한 dev 후보를 전체 검증하고, 같은 HEAD만 공식 게시 경로로 배포한다."""

import argparse
import contextlib
from datetime import datetime, timedelta
import fcntl
import json
import os
from pathlib import Path
import plistlib
import re
import shlex
import shutil
import signal
import subprocess
import sys
import time

LABEL = "dev.krill.nightly"
SUPPORT = Path.home() / "Library/Application Support" / LABEL
CONFIG = SUPPORT / "config.json"
LOGS = Path.home() / "Library/Logs/Krill/nightly"
PLIST = Path.home() / "Library/LaunchAgents" / f"{LABEL}.plist"
VOLUME = Path("/Volumes/krill-build")
TARGETS = VOLUME / "targets"
AUTHORIZATION = "사용자 지시: 매일 한국시간 자정 dev 전체 검증 후 main·태그·릴리스 자동 배포"


class NightlyError(RuntimeError):
    pass


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")
    temporary.replace(path)


def command(args, cwd=None, env=None, timeout=300):
    args = [str(arg) for arg in args]
    print(f"$ {shlex.join(args)}", flush=True)
    # 빌드·검사는 로그 파일에 바로 흘려 중간 진행 상황도 볼 수 있게 한다.
    capture = timeout <= 300
    child = subprocess.Popen(args, cwd=cwd, env=env, stdout=subprocess.PIPE if capture else sys.stdout,
                             stderr=subprocess.STDOUT, start_new_session=True)
    try:
        output, _ = child.communicate(timeout=timeout)
    except BaseException:
        with contextlib.suppress(ProcessLookupError):
            os.killpg(child.pid, signal.SIGTERM)
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
        with contextlib.suppress(ProcessLookupError):
            os.killpg(child.pid, signal.SIGKILL)
        raise
    text = output.decode("utf-8", errors="replace") if output is not None else ""
    if capture:
        print(text, end="" if text.endswith("\n") else "\n", flush=True)
    if child.returncode:
        raise NightlyError(f"command failed ({child.returncode}): {shlex.join(args)}")
    return text.strip()


def version_tuple(version):
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise NightlyError(f"expected stable semantic version, got {version}")
    return tuple(map(int, version.split(".")))


def bump_version(current, published):
    major, minor, patch = max(version_tuple(current), version_tuple(published))
    return f"{major}.{minor}.{patch + 1}"


def workflow_results(runs, sha, branch, expected):
    # API 순서에 기대지 않고 같은 워크플로의 최신 실행·재시도를 고른다.
    latest = {}
    for run in runs:
        name = Path(run["path"].split("@")[0]).name
        if run["head_sha"] != sha or run["head_branch"] != branch or name not in expected:
            continue
        previous = latest.get(name)
        if previous is None or (run["id"], run.get("run_attempt", 1)) > (
                previous["id"], previous.get("run_attempt", 1)):
            latest[name] = run
    for name, run in latest.items():
        if run["status"] == "completed" and run["conclusion"] != "success":
            raise NightlyError(f"{name}: {run['conclusion']} ({run['html_url']})")
    if len(latest) != len(expected) or any(run["status"] != "completed" for run in latest.values()):
        return None
    return latest


def launch_agent(runner, config, logs):
    return {
        "Label": LABEL,
        "ProgramArguments": ["/usr/bin/python3", str(runner), "run", "--config", str(config)],
        "StartCalendarInterval": {"Hour": 0, "Minute": 0},
        "ProcessType": "Background",
        "LowPriorityIO": True,
        "Nice": 10,
        "EnvironmentVariables": {"PATH": "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
                                 "PYTHONUNBUFFERED": "1"},
        "StandardOutPath": str(logs / "launchd.log"),
        "StandardErrorPath": str(logs / "launchd-error.log"),
    }


def preflight(config):
    if sys.platform != "darwin":
        raise NightlyError("nightly release requires macOS")
    if datetime.now().astimezone().utcoffset() != timedelta(hours=9):
        raise NightlyError("launchd calendar must run in the Mac's Asia/Seoul timezone")
    if not os.path.ismount(VOLUME):
        raise NightlyError(f"external build volume is not mounted: {VOLUME}")
    root = Path(config["run_root"]).resolve()
    target = Path(config["target_dir"]).resolve()
    if VOLUME not in root.parents or TARGETS not in target.parents:
        raise NightlyError("nightly work and Cargo targets must stay on /Volumes/krill-build")
    source = Path(config["source"])
    if not source.is_dir():
        raise NightlyError(f"source repository is missing: {source}")
    for name in ("git", "gh", "cargo", "cargo-nextest", "cargo-deny", "zsh", "lockf", "codesign"):
        if not shutil.which(name):
            raise NightlyError(f"required tool is missing: {name}")
    env = dict(os.environ, CARGO_TARGET_DIR=str(target))
    command(["gh", "api", "user", "--jq", ".login"], env=env)
    command(["git", "check-ref-format", "--branch", config["branch"]], cwd=source)
    command(["git", "rev-parse", "--verify", f"refs/heads/{config['branch']}^{{commit}}"], cwd=source)
    origin = command(["git", "remote", "get-url", "origin"], cwd=source)
    match = re.fullmatch(r"(?:https://github\.com/|git@github\.com:)([\w.-]+/[\w.-]+?)(?:\.git)?", origin)
    if not match or match[1] != config["repo"]:
        raise NightlyError("source origin does not match the configured GitHub repository")
    key = Path(config["key_file"])
    if not key.is_file():
        raise NightlyError(f"existing update signing key is missing: {key}")
    public_key = command(["/usr/bin/python3", source / "packaging/macos/ed25519_manifest.py",
                          "keygen", "--key-file", key], env=env)
    if not re.fullmatch(r"[0-9a-f]{64}", public_key):
        raise NightlyError("invalid update public key")
    print("Preflight passed (no build, push, tag, or release).", flush=True)
    return public_key


class Release:
    def __init__(self, config, state_dir):
        self.config = config
        self.state_dir = state_dir
        self.root = Path(config["run_root"])
        self.work = self.root / "repository"
        self.env = dict(os.environ, CARGO_TARGET_DIR=config["target_dir"],
                        KRILL_ALLOW_PROTECTED_PUSH=AUTHORIZATION,
                        KRILL_UPDATE_KEY_FILE=config["key_file"], CARGO_BUILD_JOBS="4")
        # 예약 실행은 유료 VM이나 사람 전용 가드 탈출구를 상속하지 않는다.
        for key in ("KRILL_GCP_PROJECT", "KRILL_WIN_VM_DIR", "KRILL_PUBLISH_FORCE",
                    "KRILL_RELEASE_SIGNING_KEY", "KRILL_RELEASE_ASSET_DIR", "KRILL_PUBLISH_LOCKED"):
            self.env.pop(key, None)
        self.state = {"started_at": datetime.now().astimezone().isoformat(), "status": "running"}

    def update(self, **values):
        self.state.update(values)
        write_json(self.state_dir / "status.json", self.state)

    def run(self, *args, timeout=300):
        return command(args, cwd=self.work, env=self.env, timeout=timeout)

    def git(self, *args):
        return self.run("git", *args)

    def ancestor(self, older, newer):
        result = subprocess.run(["git", "merge-base", "--is-ancestor", older, newer],
                                cwd=self.work, env=self.env, capture_output=True)
        if result.returncode not in (0, 1):
            raise NightlyError(result.stderr.decode(errors="replace"))
        return result.returncode == 0

    def wait_ci(self, sha, branch, workflows):
        deadline = time.monotonic() + 7200
        while time.monotonic() < deadline:
            data = json.loads(self.run("gh", "api", f"repos/{self.config['repo']}/actions/runs"
                                      f"?head_sha={sha}&branch={branch}&per_page=100"))
            results = workflow_results(data["workflow_runs"], sha, branch, workflows)
            if results is not None:
                return results
            time.sleep(30)
        raise NightlyError(f"CI timed out at {sha} ({branch})")

    def prepare(self):
        source = self.config["source"]
        local = command(["git", "rev-parse", f"refs/heads/{self.config['branch']}^{{commit}}"], cwd=source)
        self.root.mkdir(parents=True, exist_ok=True)
        if not self.work.exists():
            command(["git", "init", "--initial-branch=nightly", self.work])
            self.git("config", "krill.nightlyOwned", "true")
            self.git("remote", "add", "origin", f"https://github.com/{self.config['repo']}.git")
        if self.git("config", "--get", "krill.nightlyOwned") != "true":
            raise NightlyError("refusing to reset a repository not created by this scheduler")
        # 개발 체크아웃은 읽기만 하고 reset/clean은 전용 clone에만 실행한다.
        self.git("reset", "--hard")
        self.git("clean", "-fdx")
        self.git("fetch", "--prune", "--tags", "origin")
        self.git("fetch", source, f"+{local}:refs/nightly/source")
        main = self.git("rev-parse", "origin/main")
        remote = self.git("rev-parse", f"origin/{self.config['branch']}")
        latest = json.loads(self.run("gh", "release", "view", "--repo", self.config["repo"],
                                    "--json", "tagName"))["tagName"]
        published = self.git("rev-parse", f"refs/tags/{latest}^{{commit}}")
        self.update(source_head=local, remote_source_head=remote, base_main=main, previous_release=latest)
        if all(self.ancestor(sha, published) for sha in (local, remote, main)):
            self.update(status="skipped", reason="no committed changes since the last published release")
            return None
        branch = "release/nightly-" + datetime.now().strftime("%Y%m%d-%H%M%S")
        self.git("checkout", "-B", branch, main)
        for field in ("user.name", "user.email"):
            value = command(["git", "config", "--get", field], cwd=source)
            self.git("config", field, value)
        self.git("config", "core.hooksPath", "scripts/hooks")
        self.git("merge", "--no-edit", local, remote)
        manifest = self.work / "Cargo.toml"
        text = manifest.read_text()
        pattern = r'(?ms)(^\[workspace\.package\]\n.*?^version = ")([^"\n]+)(")'
        match = re.search(pattern, text)
        if match is None:
            raise NightlyError("workspace version is missing")
        version = bump_version(match[2], latest.removeprefix("v"))
        manifest.write_text(re.sub(pattern, lambda item: item[1] + version + item[3], text, count=1))
        self.run("cargo", "update", "--workspace", "--offline")
        self.run("python3", "docs/licensing/generate_notices.py", timeout=900)
        self.git("add", "Cargo.toml", "Cargo.lock", "THIRD_PARTY_NOTICES.md")
        self.git("commit", "-m", f"Release v{version}\n\nNightly-Source: {local}\nNightly-Remote: {remote}")
        sha = self.git("rev-parse", "HEAD")
        self.update(candidate_head=sha, candidate_branch=branch, version=version)
        return sha, branch, version, main

    def publish(self):
        public_key = preflight(self.config)
        prepared = self.prepare()
        if prepared is None:
            return
        sha, branch, version, main = prepared
        self.update(step="full-checks")
        receipt = self.state_dir / "checks" / f"{sha}.json"
        if not receipt.exists():
            self.run("/usr/bin/python3", "packaging/test_nightly.py")
            self.run("/usr/bin/python3", "packaging/test_publish.py")
            self.run("/bin/zsh", "scripts/check.sh", "--memory", timeout=14400)
            self.run("/bin/bash", "scripts/check-windows.sh", "--clippy", "--target-dir",
                     TARGETS / "nightly-windows", timeout=7200)
            self.run("cargo", "build", "--workspace", "--bins", "--release", "--locked", timeout=3600)
            self.run("cargo", "test", "--release", "--locked", "-p", "krill-app", "--test", "pane_e2e",
                     "release_survival::", "--", "--ignored", "--nocapture", "--test-threads=1", timeout=3600)
            self.run("cargo", "test", "--release", "--locked", "-p", "krill-holder", "--test", "survive",
                     "--", "--nocapture", "--test-threads=1", timeout=1800)
            self.git("diff", "--exit-code", "HEAD")
            write_json(receipt, {"head": sha, "scope": "check.sh --memory; Windows clippy; release app/holder survival",
                                 "passed_at": datetime.now().astimezone().isoformat()})
        self.run("/bin/bash", "scripts/public-surface.sh")
        self.git("push", "origin", f"HEAD:refs/heads/{branch}")
        self.update(step="native-builds-and-ci")
        self.run("gh", "workflow", "run", "release.yml", "--repo", self.config["repo"], "--ref", branch,
                 "-f", f"tag=v{version}", "-f", "build_only=true", "-f", f"public_key={public_key}")
        results = self.wait_ci(sha, branch, {"ci.yml", "ci-macos.yml", "release.yml"})
        build_id = results["release.yml"]["id"]
        assets = self.root / "assets" / sha
        if assets.exists():
            shutil.rmtree(assets)
        for name in (f"krill-windows-{version}", f"krill-linux-{version}-x86_64", f"krill-linux-{version}-aarch64"):
            self.run("gh", "run", "download", str(build_id), "--repo", self.config["repo"],
                     "--dir", assets, "-n", name, timeout=900)
        # 설치 때 검증한 도우미로 받은 산출물을 다시 확인한다.
        from nightly_assets import import_assets
        verified = self.root / "verified" / sha
        if verified.exists():
            shutil.rmtree(verified)
        import_assets(assets, verified, version, sha, public_key)
        self.update(step="promote-main", native_build_run=build_id)
        self.git("fetch", "origin", "main")
        if self.git("rev-parse", "origin/main") != main:
            raise NightlyError("main changed during validation; candidate retained, retry next night")
        if self.git("rev-parse", "HEAD") != sha or self.git("status", "--porcelain"):
            raise NightlyError("candidate changed after validation")
        self.git("push", "origin", "HEAD:refs/heads/main")
        self.wait_ci(sha, "main", {"ci.yml", "ci-macos.yml"})
        self.update(step="publish")
        self.env["KRILL_RELEASE_ASSET_DIR"] = str(verified)
        self.run("/bin/bash", "packaging/publish-github.sh", "--repo", self.config["repo"], timeout=7200)
        self.update(status="published", step="done", tag=f"v{version}",
                    finished_at=datetime.now().astimezone().isoformat())


def run_release(config, state_dir):
    state_dir.mkdir(parents=True, exist_ok=True)
    with (state_dir / "run.lock").open("w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print("Another nightly release is running; skipped.")
            return
        LOGS.mkdir(parents=True, exist_ok=True)
        log = LOGS / (datetime.now().strftime("%Y%m%d-%H%M%S") + ".log")
        with log.open("a", buffering=1) as stream, contextlib.redirect_stdout(stream), contextlib.redirect_stderr(stream):
            release = Release(config, state_dir)
            release.update(log=str(log))
            try:
                release.publish()
            except BaseException as error:
                release.update(status="failed", error=str(error), finished_at=datetime.now().astimezone().isoformat())
                print(f"Nightly stopped: {error}", flush=True)
                raise


def install(source):
    SUPPORT.mkdir(parents=True, exist_ok=True)
    with (SUPPORT / "run.lock").open("w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise NightlyError("nightly is running; install after it finishes") from error
        install_locked(source)


def install_locked(source):
    source = source.resolve()
    origin = command(["git", "remote", "get-url", "origin"], cwd=source)
    match = re.fullmatch(r"(?:https://github\.com/|git@github\.com:)([\w.-]+/[\w.-]+?)(?:\.git)?", origin)
    if not match:
        raise NightlyError("origin must be a GitHub repository")
    config = {"source": str(source), "branch": "dev", "repo": match[1],
              "run_root": str(VOLUME / "nightly"), "target_dir": str(TARGETS / "nightly"),
              "key_file": str(Path.home() / "Library/Keychains/minikrill-codesign.keychain-db.ed25519.key")}
    preflight(config)
    SUPPORT.mkdir(parents=True, exist_ok=True)
    LOGS.mkdir(parents=True, exist_ok=True)
    for name in ("nightly.py", "nightly_assets.py"):
        shutil.copyfile(source / "packaging" / name, SUPPORT / name)
    write_json(CONFIG, config)
    CONFIG.chmod(0o600)
    PLIST.parent.mkdir(parents=True, exist_ok=True)
    domain = f"gui/{os.getuid()}"
    # 기존 잡이 있으면 교체한다. 설치 즉시 배포를 실행하지 않는다.
    loaded = subprocess.run(["launchctl", "print", f"{domain}/{LABEL}"], capture_output=True).returncode == 0
    if loaded:
        command(["launchctl", "bootout", f"{domain}/{LABEL}"])
    with PLIST.open("wb") as stream:
        plistlib.dump(launch_agent(SUPPORT / "nightly.py", CONFIG, LOGS), stream)
    command(["plutil", "-lint", PLIST])
    command(["launchctl", "enable", f"{domain}/{LABEL}"])
    command(["launchctl", "bootstrap", domain, PLIST])
    print(f"Installed {LABEL}: every day at 00:00 Asia/Seoul; first release runs on schedule.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("install", "preflight", "run", "status", "uninstall"))
    parser.add_argument("--source", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--config", type=Path, default=CONFIG)
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda *_: sys.exit("nightly received SIGTERM"))
    if args.action == "install":
        install(args.source)
    elif args.action == "uninstall":
        command(["launchctl", "bootout", f"gui/{os.getuid()}/{LABEL}"])
        PLIST.unlink(missing_ok=True)
        print("Nightly schedule removed; logs and receipts retained.")
    elif args.action == "status":
        command(["launchctl", "print", f"gui/{os.getuid()}/{LABEL}"])
        status = args.config.parent / "status.json"
        print(status.read_text() if status.exists() else "Waiting for first scheduled run.")
    else:
        config = json.loads(args.config.read_text())
        if args.action == "preflight":
            preflight(config)
        else:
            run_release(config, args.config.parent)


if __name__ == "__main__":
    main()
