"""Regression checks for archived notices and SPDX fallbacks (no Cargo/network)."""

from pathlib import Path
import tempfile
import unittest

from generate_notices import notice_files, synthesized_notices


class NoticeTests(unittest.TestCase):
    def test_all_declared_alternatives_have_standard_terms(self):
        notices = dict(synthesized_notices({"license": "Zlib OR (MIT AND Apache-2.0)"}))
        self.assertEqual(set(notices), {"SPDX:Apache-2.0", "SPDX:MIT", "SPDX:Zlib"})
        self.assertIn("Copyright (c) <year> <copyright holders>", notices["SPDX:MIT"])
        self.assertNotIn("Krill contributors", notices["SPDX:Apache-2.0"])
        self.assertIn("1. Definitions.", notices["SPDX:Apache-2.0"])
        self.assertIn("3. This notice may not be removed", notices["SPDX:Zlib"])

    def test_unknown_identifiers_and_exceptions_are_not_partial_notices(self):
        for expression in ("MIT-0", "MIT OR BSD-3-Clause", "Apache-2.0 WITH LLVM-exception", ""):
            with self.subTest(expression=expression):
                self.assertEqual(synthesized_notices({"license": expression}), [])

    def test_authors_do_not_become_invented_copyright_holders(self):
        notices = synthesized_notices({"license": "MIT", "authors": ["Package maintainer"]})
        self.assertNotIn("Package maintainer", notices[0][1])

    def test_missing_standalone_file_uses_spdx_terms(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "Cargo.toml"
            manifest.write_text('[package]\nlicense = "MIT"\n')
            notices = notice_files({"manifest_path": str(manifest), "license": "MIT"})
            self.assertEqual(notices[0][0], "SPDX:MIT")

    def test_upstream_notice_preserves_copyright_and_has_priority(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "Cargo.toml"
            manifest.touch()
            (manifest.parent / "LICENSE").write_text("Copyright 2001 Upstream  \nTerms.\n")
            notices = notice_files({"manifest_path": str(manifest), "license": "MIT"})
            self.assertEqual(notices, [("LICENSE", "Copyright 2001 Upstream\nTerms.\n")])


if __name__ == "__main__":
    unittest.main()
