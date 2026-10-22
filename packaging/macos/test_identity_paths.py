import tempfile
import unittest
from pathlib import Path
from identity_paths import keychain_path


class IdentityPathsTests(unittest.TestCase):
    def test_aliases_reuse_every_existing_key_and_leave_new_files_alone(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            parent = home / "Library/Keychains"
            parent.mkdir(parents=True)
            for suffix in ("", ".pass", ".ed25519.key", ".ed25519.key.pub"):
                (parent / ("minikrill-codesign.keychain-db" + suffix)).write_bytes(b"existing-key")
            canonical = keychain_path(home)
            for suffix in ("", ".pass", ".ed25519.key", ".ed25519.key.pub"):
                alias = Path(str(canonical) + suffix)
                self.assertTrue(alias.is_symlink())
                self.assertEqual(alias.read_bytes(), b"existing-key")
            self.assertEqual(keychain_path(home), canonical)
            canonical.unlink()
            canonical.write_bytes(b"separate-identity")
            self.assertEqual(keychain_path(home).read_bytes(), b"separate-identity")

    def test_fresh_home_uses_new_name_without_creating_keys(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = keychain_path(Path(tmp))
            self.assertEqual(path.name, "krill-codesign.keychain-db")
            self.assertFalse(path.exists())


if __name__ == "__main__":
    unittest.main()
