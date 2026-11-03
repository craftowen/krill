#!/usr/bin/env python3
"""GitHub workflow supply-chain and trigger contracts that actionlint does not check."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = sorted((ROOT / ".github/workflows").glob("*.yml"))


class WorkflowTests(unittest.TestCase):
    def test_external_actions_are_pinned_to_a_commit_sha(self):
        # .github/dependabot.yml policy: tags can be moved, commit SHAs cannot.
        self.assertTrue(WORKFLOWS)
        for workflow in WORKFLOWS:
            for number, line in enumerate(workflow.read_text().splitlines(), 1):
                match = re.match(r"\s*(?:-\s*)?uses:\s*(\S+)", line)
                if not match or match[1].startswith("./"):
                    continue
                with self.subTest(workflow=workflow.name, line=number):
                    self.assertRegex(match[1], r"^[\w.-]+/[\w./-]+@[0-9a-f]{40}$")


if __name__ == "__main__":
    unittest.main()
