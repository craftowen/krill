import tempfile
import unittest
from pathlib import Path
from identity_paths import keychain_path


class IdentityPathsTests(unittest.TestCase):
    def test_fresh_home_uses_krill_name_without_creating_keys(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = keychain_path(Path(tmp))
            self.assertEqual(path.name, "krill-codesign.keychain-db")
            self.assertFalse(path.exists())


if __name__ == "__main__":
    unittest.main()
