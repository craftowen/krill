#!/usr/bin/env python3
"""전체 로컬 게이트와 출고 바이너리 검증을 한 번 실행하고 서명된 증거를 남긴다."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

from macos import ed25519_manifest as ed

VOLUME = Path("/Volumes/krill-build")
TARGETS = VOLUME / "targets"
BINARIES = ("krill-app", "krill", "krill-holder", "krilld", "krill-link")
SCOPE = ("scripts/check.sh", "windows-clippy", "memory-10-terminals", "release-gui-kill9-replay", "release-holder-survival")
SCHEMA = 1


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def target_directory(configured=None, volume=VOLUME):
    if not os.path.ismount(volume):
        raise ValueError("external build volume is not mounted; no internal target fallback")
    if (volume / "targets").resolve().parent != volume.resolve():
        raise ValueError("the target root must not redirect outside the mounted volume")
    target = (volume / "targets/krill-publish").resolve()
    if configured and Path(configured).is_absolute():
        target = (Path(configured).parent / "krill-publish").resolve()
    if (volume / "targets").resolve() not in target.parents:
        raise ValueError("publish target must stay inside /Volumes/krill-build/targets")
    return target


def fingerprint():
    names = ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
             "CC", "CXX", "CFLAGS", "CXXFLAGS", "LDFLAGS", "SDKROOT", "MACOSX_DEPLOYMENT_TARGET")
    flags = {key: value for key, value in os.environ.items() if key in names or key.startswith(("CARGO_PROFILE_", "CARGO_TARGET_"))}
    config = Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo"))) / "config.toml"
    flags["cargo-config-sha256"] = sha256(config) if config.is_file() else None
    return {"rustc": subprocess.check_output(["rustc", "-Vv"], text=True).strip(),
            "flags_sha256": hashlib.sha256(json.dumps(flags, sort_keys=True).encode()).hexdigest()}


def payload(proof):
    unsigned = {key: value for key, value in proof.items() if key != "signature"}
    return b"krill-release-validation-v1\n" + json.dumps(unsigned, sort_keys=True, separators=(",", ":")).encode()


def sign(proof, key_file):
    seed = ed.load_or_create_seed(Path(key_file), create_if_missing=False)
    proof["public_key"] = ed.public_key_from_seed(seed).hex()
    proof["signature"] = ed.ed25519_sign(seed, payload(proof)).hex()
    return proof


def attestation(proof, head, key_file, scope, expected_fingerprint=None):
    seed = ed.load_or_create_seed(Path(key_file), create_if_missing=False)
    public_key = ed.public_key_from_seed(seed)
    if proof.get("public_key") != public_key.hex() or not ed.ed25519_verify(
            public_key, payload(proof), bytes.fromhex(proof.get("signature", ""))):
        raise ValueError("release validation signature is invalid")
    if proof.get("schema") != SCHEMA or proof.get("head") != head or proof.get("scope") != list(scope):
        raise ValueError("release proof SHA, policy, or validation scope does not match")
    age = (datetime.now(timezone.utc) - datetime.fromisoformat(proof["passed_at"])).total_seconds()
    if age < -300 or age > 86400:
        raise ValueError("release proof is outside its 24-hour validity window")
    if proof.get("fingerprint") != (expected_fingerprint if expected_fingerprint is not None else fingerprint()):
        raise ValueError("release toolchain or compiler flags changed after validation")


def verify(receipt, head, key_file, allowed_root=VOLUME, expected_fingerprint=None):
    receipt = Path(receipt)
    if receipt.is_symlink() or allowed_root.resolve() not in receipt.resolve().parents:
        raise ValueError("release proof must be a local regular file on the external build volume")
    proof = json.loads(receipt.read_text())
    attestation(proof, head, key_file, SCOPE, expected_fingerprint)
    directory = receipt.parent / "bin"
    if directory.is_symlink() or set(proof.get("binaries", {})) != set(BINARIES):
        raise ValueError("release binary inventory does not match")
    for binary in BINARIES:
        path = directory / binary
        if path.is_symlink() or not path.is_file() or sha256(path) != proof["binaries"][binary]:
            raise ValueError(f"tested release binary changed: {binary}")
    return directory


def run(args, capture=False, timeout=14400):
    print("$ " + " ".join(map(str, args)), file=sys.stderr, flush=True)
    result = subprocess.run(list(map(str, args)), stdout=subprocess.PIPE if capture else None,
                            text=True, timeout=timeout, check=True)
    return result.stdout or ""


def test_executable(package, target):
    output = run(["cargo", "test", "--release", "--locked", "-p", package, "--test", target,
                  "--no-run", "--message-format=json"], capture=True)
    executables = []
    for line in output.splitlines():
        item = json.loads(line)
        if item.get("reason") == "compiler-message":
            print(item["message"].get("rendered", ""), file=sys.stderr)
        if item.get("reason") == "compiler-artifact" and item.get("target", {}).get("name") == target and item.get("executable"):
            executables.append(Path(item["executable"]))
    if len(executables) != 1:
        raise ValueError(f"expected one {target} test executable")
    return executables[0]


def validate(head, directory, key_file):
    directory = Path(directory)
    if VOLUME not in directory.resolve().parents or directory.exists():
        raise ValueError("release staging must be a fresh directory on the external volume")
    target = Path(os.environ["CARGO_TARGET_DIR"]).resolve()
    if TARGETS not in target.parents or not os.path.ismount(VOLUME):
        raise ValueError("all release validation targets must stay on the external volume")
    if run(["git", "rev-parse", "HEAD"], capture=True).strip() != head:
        raise ValueError("validation source HEAD does not match")
    run(["git", "diff", "--exit-code", "HEAD"])
    # 전체 check.sh는 최종 후보에서 한 번만. 메모리 검사는 아래에서 출고 바이너리에 직접 실행한다.
    checkpoint = directory.parent / f"{head}-full-check.json"
    if checkpoint.exists():
        attestation(json.loads(checkpoint.read_text()), head, key_file, ("scripts/check.sh",))
        print("Reusing signed full-check checkpoint for the same SHA and toolchain.", file=sys.stderr)
    else:
        run(["/bin/zsh", "scripts/check.sh"])
        checkpoint.parent.mkdir(parents=True, exist_ok=True)
        checkpoint.write_text(json.dumps(sign({"schema": SCHEMA, "head": head, "scope": ["scripts/check.sh"],
                                              "fingerprint": fingerprint(), "passed_at": datetime.now(timezone.utc).isoformat()}, key_file)))
        checkpoint.chmod(0o600)
    run(["/bin/bash", "scripts/check-windows.sh", "--clippy", "--target-dir", TARGETS / "nightly-windows"], timeout=7200)
    pane = test_executable("krill-app", "pane_e2e")
    holder = test_executable("krill-holder", "survive")
    # 테스트 컴파일 뒤 최종 출고 프로필을 만든다. 이후 게이트는 Cargo 재빌드 없이 실행한다.
    build = ["cargo", "build", "--release", "--locked", "--bins"]
    for package in BINARIES:
        build.extend(("-p", package))
    run(build)
    before = {name: sha256(target / "release" / name) for name in BINARIES}
    gates = ([pane, "memory_gate::release_ten_terminals_within_200mib", "--exact", "--ignored", "--nocapture", "--test-threads=1"],
             [pane, "release_survival::release_app_kill9_survives_and_replays_hash_three_times", "--exact", "--ignored", "--nocapture", "--test-threads=1"],
             [holder, "--nocapture", "--test-threads=1"])
    pane_tests = run([pane, "--list"], capture=True)
    holder_tests = run([holder, "--list"], capture=True)
    for test in (gates[0][1], gates[1][1]):
        if f"{test}: test" not in pane_tests.splitlines():
            raise ValueError(f"required release test is missing: {test}")
    for test in ("krilld_kill", "protocol_contract", "upgrade::exec_handoff"):
        if f"{test}: test" not in holder_tests.splitlines():
            raise ValueError(f"required holder survival test is missing: {test}")
    for gate in gates:
        run(gate, timeout=900)
        if before != {name: sha256(target / "release" / name) for name in BINARIES}:
            raise ValueError("release binaries changed while validation was running")
    run(["git", "diff", "--exit-code", "HEAD"])
    binary_dir = directory / "bin"
    binary_dir.mkdir(parents=True, mode=0o700)
    for name in BINARIES:
        shutil.copyfile(target / "release" / name, binary_dir / name)
        (binary_dir / name).chmod(0o500)
    proof = {"schema": SCHEMA, "head": head, "scope": list(SCOPE), "binaries": before,
             "fingerprint": fingerprint(), "passed_at": datetime.now(timezone.utc).isoformat()}
    sign(proof, key_file)
    receipt = directory / "validation.json"
    receipt.write_text(json.dumps(proof, indent=2) + "\n")
    receipt.chmod(0o400)
    verify(receipt, head, key_file, allowed_root=VOLUME)
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("target-dir", "validate", "verify"))
    parser.add_argument("--head")
    parser.add_argument("--directory", type=Path)
    parser.add_argument("--receipt", type=Path)
    parser.add_argument("--key-file", type=Path)
    parser.add_argument("--base")
    args = parser.parse_args()
    if args.action == "target-dir":
        print(target_directory(args.base))
    elif args.action == "verify":
        print(verify(args.receipt, args.head, args.key_file))
    else:
        print(validate(args.head, args.directory, args.key_file))


if __name__ == "__main__":
    main()
