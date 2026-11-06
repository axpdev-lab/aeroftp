#!/usr/bin/env python3
"""Every actions/checkout step must set persist-credentials: false.

A persisted checkout token stays in .git/config for every later step of the
job, including third-party actions and scripts that never needed it. No
workflow here pushes, fetches or pulls after its checkout; `gh` reads GH_TOKEN
from the environment. A workflow that does need the persisted token goes in
ALLOWED, with the reason, so the exception is reviewed, not silent.

Read as text, not YAML: PyYAML is not guaranteed on every runner, and the
step shapes used here are the two that GitHub documents.
"""
from pathlib import Path
import re
import unittest

WORKFLOWS = Path(__file__).resolve().parents[1] / "workflows"

# "<workflow file>:<line of the uses:>" -> why that job needs the token. Empty today.
ALLOWED = {}

USES = re.compile(r"^(\s*)(- )?uses:\s*actions/checkout@")


def checkouts_without_the_setting(text):
    """Line numbers (1-based) of checkout steps that lack persist-credentials: false."""
    lines = text.split("\n")
    missing = []
    for i, line in enumerate(lines):
        match = USES.match(line)
        if not match:
            continue
        # The step's keys sit at this indent; the step ends at the next line
        # that is less indented, or at the next `- ` item at the dash's indent.
        key_indent = len(match.group(1)) + (2 if match.group(2) else 0)
        block = []
        for nxt in lines[i + 1:]:
            stripped = nxt.strip()
            if stripped and not stripped.startswith("#"):
                indent = len(nxt) - len(nxt.lstrip())
                if indent < key_indent or (indent == key_indent - 2 and stripped.startswith("- ")):
                    break
            block.append(nxt)
        if not any(re.match(r"^\s*persist-credentials:\s*false\s*(#.*)?$", b) for b in block):
            missing.append(i + 1)
    return missing


class CheckoutCredentials(unittest.TestCase):
    def test_scanner_reads_both_step_shapes(self):
        good = (
            "    steps:\n"
            "      - uses: actions/checkout@0123456789012345678901234567890123456789 # v5\n"
            "        with:\n"
            "          fetch-depth: 0\n"
            "          persist-credentials: false\n"
            "      - name: Checkout\n"
            "        uses: actions/checkout@0123456789012345678901234567890123456789 # v5\n"
            "        with:\n"
            "          persist-credentials: false\n"
        )
        self.assertEqual(checkouts_without_the_setting(good), [])
        bad = (
            "    steps:\n"
            "      - uses: actions/checkout@0123456789012345678901234567890123456789 # v5\n"
            "      - name: Next step\n"
            "        with:\n"
            "          persist-credentials: false\n"
            "      - name: Checkout\n"
            "        uses: actions/checkout@0123456789012345678901234567890123456789 # v5\n"
            "        with:\n"
            "          persist-credentials: true\n"
        )
        # The setting on the NEXT step does not count for the first checkout.
        self.assertEqual(checkouts_without_the_setting(bad), [2, 7])

    def test_every_checkout_drops_the_token(self):
        files = sorted(WORKFLOWS.glob("*.yml")) + sorted(WORKFLOWS.glob("*.yaml"))
        self.assertTrue(files, "no workflow files found")
        seen = 0
        offenders = []
        for path in files:
            text = path.read_text()
            seen += sum(1 for line in text.split("\n") if USES.match(line))
            for line in checkouts_without_the_setting(text):
                key = f"{path.name}:{line}"
                if key not in ALLOWED:
                    offenders.append(key)
        self.assertGreater(seen, 0, "no actions/checkout step found: the scanner is reading nothing")
        self.assertEqual(offenders, [], "add `persist-credentials: false` under `with:`, or justify the step in ALLOWED")


if __name__ == "__main__":
    unittest.main()
