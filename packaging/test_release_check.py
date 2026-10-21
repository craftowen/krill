#!/usr/bin/env python3
"""전체 검사 증거·바이너리 변조·0개 테스트·재개·외장 디스크 제한 회귀."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import release_check as rc

HEAD = "a" * 40
FP = {"rustc": "test-toolchain", "flags_sha256": "b" * 64}


class ProofTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.key = self.root / "update.key"
        self.key.write_text(os.urandom(32).hex())
        self.directory = self.root / "stage"
        self.directory.mkdir()
        self.bin = self.directory / "bin"
        self.bin.mkdir()
        for name in rc.BINARIES:
            (self.bin / name).write_bytes(name.encode())
        self.receipt = self.directory / "validation.json"
        self.proof = {"schema": rc.SCHEMA, "head": HEAD, "scope": list(rc.SCOPE), "fingerprint": FP,
                      "passed_at": rc.datetime.now(rc.timezone.utc).isoformat(),
                      "binaries": {name: rc.sha256(self.bin / name) for name in rc.BINARIES}}
        self.save()

    def save(self):
        self.proof.pop("signature", None)
        self.receipt.write_text(json.dumps(rc.sign(self.proof, self.key)))

    def verify(self, head=HEAD, fingerprint=FP):
        return rc.verify(self.receipt, head, self.key, allowed_root=self.root, expected_fingerprint=fingerprint)

    def test_signed_receipt_checks_complete_scope_head_toolchain_and_every_frozen_binary(self):
        self.assertEqual(self.verify(), self.bin)
        with self.assertRaisesRegex(ValueError, "SHA, policy"):
            self.verify("c" * 40)
        with self.assertRaisesRegex(ValueError, "compiler flags"):
            self.verify(fingerprint={"different": True})
        (self.bin / "krilld").write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError, "binary changed"):
            self.verify()

    def test_forged_scope_and_wrong_signing_key_cannot_authorize_publication(self):
        forged = dict(self.proof, scope=["pretend-everything-passed"])
        self.receipt.write_text(json.dumps(forged))
        with self.assertRaisesRegex(ValueError, "signature"):
            self.verify()
        self.save()
        self.key.write_text(os.urandom(32).hex())
        with self.assertRaisesRegex(ValueError, "signature"):
            self.verify()

    def test_expired_receipt_and_symlink_inventory_are_refused(self):
        self.proof["passed_at"] = "2000-01-01T00:00:00+00:00"
        self.save()
        with self.assertRaisesRegex(ValueError, "24-hour"):
            self.verify()
        self.proof["passed_at"] = rc.datetime.now(rc.timezone.utc).isoformat()
        self.save()
        source = self.bin / "krilld"
        source.rename(self.root / "outside")
        source.symlink_to(self.root / "outside")
        with self.assertRaisesRegex(ValueError, "binary changed"):
            self.verify()

    def test_target_never_falls_back_to_internal_storage_or_redirected_root(self):
        with patch.object(rc.os.path, "ismount", return_value=False):
            with self.assertRaisesRegex(ValueError, "not mounted"):
                rc.target_directory(volume=self.root)
        with patch.object(rc.os.path, "ismount", return_value=True):
            self.assertEqual(rc.target_directory(volume=self.root), self.root / "targets/krill-publish")
            with self.assertRaisesRegex(ValueError, "must stay inside"):
                rc.target_directory("/tmp/shared", volume=self.root)
            (self.root / "targets").symlink_to("/tmp")
            with self.assertRaisesRegex(ValueError, "redirect"):
                rc.target_directory(volume=self.root)


class ValidationTests(unittest.TestCase):
    def test_canonical_build_is_tested_without_cargo_rebuilding_it_and_full_gate_resumes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            target = root / "targets/krill-publish"
            (target / "release").mkdir(parents=True)
            key = root / "key"
            key.write_text(os.urandom(32).hex())
            calls = []
            state = {"fail_memory": True, "missing": False, "mutate": False}

            def run(args, **kwargs):
                args = list(map(str, args))
                calls.append(args)
                if args[:3] == ["git", "rev-parse", "HEAD"]:
                    return HEAD
                if args[:2] == ["cargo", "test"]:
                    name = args[args.index("--test") + 1]
                    return json.dumps({"reason": "compiler-artifact", "target": {"name": name}, "executable": str(target / name)})
                if args[:2] == ["cargo", "build"]:
                    for name in rc.BINARIES:
                        (target / "release" / name).write_bytes(b"canonical-" + name.encode())
                if args[-1] == "--list":
                    if args[0].endswith("pane_e2e"):
                        return "release_survival::release_app_kill9_survives_and_replays_hash_three_times: test\n" + (
                            "" if state["missing"] else "memory_gate::release_ten_terminals_within_200mib: test\n")
                    return "krilld_kill: test\nprotocol_contract: test\nupgrade::exec_handoff: test\n"
                if "memory_gate::release_ten_terminals_within_200mib" in args:
                    if state["fail_memory"]:
                        raise subprocess.CalledProcessError(1, args)
                    if state["mutate"]:
                        (target / "release/krilld").write_bytes(b"another-build")
                return ""

            with patch.object(rc, "VOLUME", root), patch.object(rc, "TARGETS", root / "targets"), \
                    patch.object(rc.os.path, "ismount", return_value=True), patch.object(rc, "fingerprint", return_value=FP), \
                    patch.object(rc, "run", side_effect=run), patch.dict(os.environ, {"CARGO_TARGET_DIR": str(target)}):
                with self.assertRaises(subprocess.CalledProcessError):
                    rc.validate(HEAD, root / "stage1", key)
                self.assertFalse((root / "stage1/validation.json").exists())
                state["fail_memory"] = False
                receipt = rc.validate(HEAD, root / "stage2", key)
                self.assertEqual(sum("scripts/check.sh" in call for call in calls), 1)
                self.assertEqual(rc.verify(receipt, HEAD, key, allowed_root=root), root / "stage2/bin")
                state["missing"] = True
                with self.assertRaisesRegex(ValueError, "test is missing"):
                    rc.validate(HEAD, root / "stage3", key)
                state.update(missing=False, mutate=True)
                with self.assertRaisesRegex(ValueError, "binaries changed"):
                    rc.validate(HEAD, root / "stage4", key)


if __name__ == "__main__":
    unittest.main()
