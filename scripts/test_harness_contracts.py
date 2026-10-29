#!/usr/bin/env python3
"""The local fast/latency split must match nextest's serial latency group."""

from pathlib import Path
import json
import re
import shlex
import unittest


ROOT = Path(__file__).resolve().parent.parent


def quoted_value(raw):
    literal = re.fullmatch(r"'([^']*)'\s*(?:#.*)?", raw)
    if literal:
        return literal[1]
    basic = re.fullmatch(r'("(?:[^"\\]|\\.)*")\s*(?:#.*)?', raw)
    if basic:
        return json.loads(basic[1])
    raise ValueError("parity guard requires single-line quoted filter/test-group values")


def latency_overrides(config):
    # Read entire override tables, independent of key order/comments/other fields.
    # This guard deliberately accepts the repository's canonical table headers and
    # single-line fields; unsupported TOML forms fail closed instead of being skipped.
    overrides = []
    current = None
    for line in config.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("["):
            header = re.fullmatch(r"(\[\[?)([A-Za-z0-9_.-]+)(\]\]?)\s*(?:#.*)?", line)
            if not header or len(header[1]) != len(header[3]):
                raise ValueError("parity guard requires canonical TOML table headers")
            current = {} if header[1] == "[[" and header[2] == "profile.default.overrides" else None
            if current is not None:
                overrides.append(current)
        elif current is not None:
            field = re.fullmatch(r'''([A-Za-z0-9_-]+|'[^']+'|"[^"\\]*")\s*=\s*(.+)''', line)
            if not field:
                raise ValueError("parity guard requires single-line override fields")
            key = quoted_value(field[1]) if field[1].startswith(("'", '"')) else field[1]
            if key in ("filter", "test-group"):
                if key in current:
                    raise ValueError("duplicate nextest override key: " + key)
                current[key] = quoted_value(field[2])
    return [entry["filter"] for entry in overrides if entry.get("test-group") == "latency"]


def latency_filters(config, script):
    groups = latency_overrides(config)
    if len(groups) != 1:
        raise ValueError("exactly one combined nextest latency override is required")
    filters = {}
    for phase in ("fast", "latency"):
        commands = re.findall(r'(?m)^[ \t]*step[ \t]+"test ' + phase + r'"[^\n]*$', script)
        if len(commands) != 1:
            raise ValueError("exactly one check.sh test phase required: " + phase)
        lexer = shlex.shlex(commands[0], posix=True, punctuation_chars=";")
        lexer.whitespace_split = True
        tokens = list(lexer)  # Shell comments are ignored; quoted '#' stays data.
        if tokens[:3] != ["step", "test " + phase, ";"]:
            raise ValueError("parity guard requires a canonical phase command: " + phase)
        flags = [i for i, token in enumerate(tokens) if token == "-E"]
        if len(flags) != 1 or flags[0] + 1 >= len(tokens):
            raise ValueError("exactly one nextest filter required: " + phase)
        filters[phase] = tokens[flags[0] + 1]
    return groups[0], filters


class FilterTests(unittest.TestCase):
    def test_fast_is_complement_of_the_serial_latency_override(self):
        expression, phases = latency_filters(
            (ROOT / ".config/nextest.toml").read_text(), (ROOT / "scripts/check.sh").read_text())
        self.assertEqual(phases["latency"], expression)
        self.assertEqual(phases["fast"], "not (" + expression + ")")

    def test_missing_phase_cannot_silently_skip_the_guard(self):
        config = "[[profile.default.overrides]]\nfilter = 'test(slow)'\ntest-group = 'latency'"
        with self.assertRaisesRegex(ValueError, "exactly one check.sh test phase"):
            latency_filters(config, "# no test phases")

    def test_multiple_latency_overrides_cannot_silently_skip_part_of_the_filter(self):
        config = "[[profile.default.overrides]]\nfilter = 'test(first)'\ntest-group = 'latency'\n"
        config += "[[profile.default.overrides]]\nfilter = 'test(second)'\ntest-group = 'latency'\n"
        with self.assertRaisesRegex(ValueError, "exactly one combined"):
            latency_filters(config, "")

    def test_extra_override_is_detected_with_intervening_key_order_comments_or_quotes(self):
        first = "[[profile.default.overrides]]\nfilter = 'test(first)'\ntest-group = 'latency'\n"
        variants = ["filter = 'test(second)'\nretries = 0\ntest-group = 'latency'",
                    "test-group = 'latency'\nfilter = 'test(second)'",
                    "filter = 'test(second)'\n# separate keys\ntest-group = 'latency'",
                    '\"test-group\" = \"latency\" # group\n\"filter\" = \"test(second)\"']
        for fields in variants:
            with self.subTest(fields=fields):
                config = first + "[[profile.default.overrides]]\n" + fields
                with self.assertRaisesRegex(ValueError, "exactly one combined"):
                    latency_filters(config, "")

    def test_unsupported_toml_forms_fail_closed(self):
        for config in ('[["profile"."default"."overrides"]]\nfilter = "test(slow)"',
                       '[[profile.default.overrides]]\nfilter = """test(slow)"""\ntest-group = "latency"'):
            with self.subTest(config=config), self.assertRaises(ValueError):
                latency_filters(config, "")

    def test_commented_phase_cannot_hide_a_changed_real_filter(self):
        config = "[[profile.default.overrides]]\nfilter = 'test(expected)'\ntest-group = 'latency'"
        script = '''# step "test fast"; slot cargo nextest -E 'not (test(expected))'
# step "test latency"; slot cargo nextest -E 'test(expected)'
step "test fast"; slot cargo nextest -E 'not (test(wrong))'
step "test latency"; slot cargo nextest -E 'test(wrong)'
'''
        expected, phases = latency_filters(config, script)
        self.assertNotEqual(phases["latency"], expected)
        self.assertNotEqual(phases["fast"], "not (" + expected + ")")

    def test_duplicate_shell_phases_fail_closed(self):
        config = "[[profile.default.overrides]]\nfilter = 'test(first)'\ntest-group = 'latency'"
        script = 'step "test fast"; slot cargo nextest -E \'not (test(first))\'\n' * 2
        with self.assertRaisesRegex(ValueError, "exactly one check.sh test phase"):
            latency_filters(config, script)

    def test_inline_comment_cannot_replace_the_real_filter(self):
        config = "[[profile.default.overrides]]\nfilter = 'test(expected)'\ntest-group = 'latency'"
        script = '''step "test fast"; slot cargo nextest -E 'not (test(wrong))' # old -E 'not (test(expected))'
step "test latency"; slot cargo nextest -E 'test(wrong)' # old -E 'test(expected)'
'''
        expected, phases = latency_filters(config, script)
        self.assertNotEqual(phases["latency"], expected)
        self.assertNotEqual(phases["fast"], "not (" + expected + ")")

    def test_quoted_hash_is_filter_data(self):
        config = "[[profile.default.overrides]]\nfilter = 'test(/#/)'\ntest-group = 'latency'"
        script = '''step "test fast"; slot cargo nextest -E 'not (test(/#/))' # comment
step "test latency"; slot cargo nextest -E 'test(/#/)' # comment
'''
        expected, phases = latency_filters(config, script)
        self.assertEqual(phases["latency"], expected)
        self.assertEqual(phases["fast"], "not (" + expected + ")")


if __name__ == "__main__":
    unittest.main()
