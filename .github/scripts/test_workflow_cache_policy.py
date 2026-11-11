#!/usr/bin/env python3
"""Workflow cache policy gate.

Enforces GitHub Actions cache quota constraints (tracker Requested 76):
1. checks.yml does not use rust-cache (does not compile application code).
2. cli-smoke.yml does not use rust-cache on any platform.
3. Every rust-cache step across all workflows enforces save-if restricted
   to refs/heads/main or disabled entirely (save-if: false).
4. build.yml uses the resilient ./.github/actions/apt-install composite action
   in both linux-validate and the Linux leg of build-and-release with
   cache-prefix: apt-tauri-dev.
5. Windows workflows purge transient whisper-rs-sys build artifacts before
   cache save to prevent packaging transient C++ build trees.
6. All workflow YAML files exist and are readable with valid job structure.

Read as text, not PyYAML: PyYAML is not guaranteed on every runner or local
Python installation (matches test_workflow_checkout_credentials.py convention).
"""
from pathlib import Path
import re
import unittest

WORKFLOWS_DIR = Path(__file__).resolve().parents[1] / "workflows"


def get_all_workflow_files():
    return sorted(WORKFLOWS_DIR.glob("*.yml"))


def parse_steps(workflow_text):
    """Parse step definitions from workflow YAML text using line-by-line scanning."""
    lines = workflow_text.split("\n")
    steps = []
    current_step = None
    in_with = False
    with_indent = None
    multiline_key = None
    multiline_indent = None

    step_start = re.compile(r"^(\s*)-\s+name:\s*(.+)$")
    step_uses_only = re.compile(r"^(\s*)-\s+uses:\s*(.+)$")

    for idx, line in enumerate(lines):
        line_num = idx + 1
        m_name = step_start.match(line)
        m_uses = step_uses_only.match(line)

        if m_name or m_uses:
            if current_step:
                steps.append(current_step)
            indent = len(line) - len(line.lstrip())
            current_step = {
                "line": line_num,
                "name": m_name.group(2).strip().strip("\"'") if m_name else "",
                "indent": indent,
                "uses": m_uses.group(2).strip() if m_uses else "",
                "if": "",
                "with": {},
                "raw_lines": [line],
            }
            in_with = False
            with_indent = None
            multiline_key = None
            continue

        if current_step:
            stripped = line.strip()
            indent = len(line) - len(line.lstrip())
            if stripped and indent <= current_step["indent"] and (
                stripped.startswith("- ") or not line.startswith(" " * (current_step["indent"] + 1))
            ):
                steps.append(current_step)
                current_step = None
                in_with = False
                with_indent = None
                multiline_key = None
                continue

            current_step["raw_lines"].append(line)
            m_uses_field = re.match(r"^\s*uses:\s*(.+)$", line)
            if m_uses_field:
                current_step["uses"] = m_uses_field.group(1).strip()
            m_if = re.match(r"^\s*if:\s*(.+)$", line)
            if m_if:
                current_step["if"] = m_if.group(1).strip().strip("\"'")

            if re.match(r"^\s*with:\s*$", line):
                in_with = True
                with_indent = indent
                multiline_key = None
                continue

            if in_with:
                if indent <= with_indent and stripped:
                    in_with = False
                    multiline_key = None
                else:
                    if multiline_key and indent > multiline_indent and stripped:
                        current_step["with"][multiline_key] += " " + stripped
                        continue
                    else:
                        multiline_key = None

                    m_key_val = re.match(r"^\s*([\w-]+):\s*(.*)$", line)
                    if m_key_val:
                        key = m_key_val.group(1)
                        val = m_key_val.group(2).strip().strip("\"'")
                        current_step["with"][key] = val
                        if val in (">-", ">", "|", "|-"):
                            multiline_key = key
                            multiline_indent = indent

    if current_step:
        steps.append(current_step)
    return steps


class TestWorkflowCachePolicy(unittest.TestCase):
    def test_all_workflows_exist_and_readable(self):
        files = get_all_workflow_files()
        self.assertGreaterEqual(len(files), 17, "Expected at least 17 workflow YAML files")
        for f in files:
            with self.subTest(workflow=f.name):
                content = f.read_text(encoding="utf-8")
                self.assertIn("jobs:", content, f"{f.name} missing 'jobs:' section")

    def test_checks_workflow_has_no_rust_cache(self):
        content = (WORKFLOWS_DIR / "checks.yml").read_text(encoding="utf-8")
        steps = parse_steps(content)
        for s in steps:
            self.assertNotIn(
                "rust-cache",
                s["uses"].lower(),
                f"checks.yml step '{s['name']}' (line {s['line']}) must not use rust-cache",
            )

    def test_cli_smoke_workflow_has_no_rust_cache(self):
        content = (WORKFLOWS_DIR / "cli-smoke.yml").read_text(encoding="utf-8")
        steps = parse_steps(content)
        for s in steps:
            self.assertNotIn(
                "rust-cache",
                s["uses"].lower(),
                f"cli-smoke.yml step '{s['name']}' (line {s['line']}) must not use rust-cache",
            )

    def test_all_rust_cache_steps_enforce_save_if_main_or_false(self):
        valid_save_if_values = {
            "${{ github.ref == 'refs/heads/main' }}",
            "false",
        }
        for f in get_all_workflow_files():
            steps = parse_steps(f.read_text(encoding="utf-8"))
            for s in steps:
                if "rust-cache" in s["uses"].lower():
                    self.assertIn(
                        "save-if",
                        s["with"],
                        f"{f.name} step '{s['name']}' (line {s['line']}) missing save-if",
                    )
                    save_if = s["with"]["save-if"]
                    self.assertIn(
                        save_if,
                        valid_save_if_values,
                        f"{f.name} step '{s['name']}' (line {s['line']}) has unapproved save-if: {save_if!r}",
                    )

    def test_build_workflow_uses_apt_install_with_correct_prefix(self):
        content = (WORKFLOWS_DIR / "build.yml").read_text(encoding="utf-8")
        steps = parse_steps(content)
        apt_steps = [s for s in steps if "./.github/actions/apt-install" in s["uses"]]

        self.assertEqual(
            len(apt_steps),
            2,
            f"Expected exactly 2 apt-install steps in build.yml, found {len(apt_steps)}",
        )

        for s in apt_steps:
            self.assertEqual(
                s["with"].get("cache-prefix"),
                "apt-tauri-dev",
                f"build.yml step '{s['name']}' (line {s['line']}) must use cache-prefix: apt-tauri-dev",
            )
            self.assertIn(
                "libwebkit2gtk-4.1-dev",
                s["with"].get("packages", ""),
                f"build.yml step '{s['name']}' (line {s['line']}) missing libwebkit2gtk-4.1-dev",
            )

    def test_windows_workflows_purge_transient_whisper_artifacts(self):
        # 1. windows-unit-tests.yml
        content_wut = (WORKFLOWS_DIR / "windows-unit-tests.yml").read_text(encoding="utf-8")
        steps_wut = parse_steps(content_wut)

        purge_wut = [s for s in steps_wut if "purge transient whisper-rs-sys" in s["name"].lower()]
        self.assertEqual(
            len(purge_wut),
            1,
            "windows-unit-tests.yml must have exactly 1 whisper purge step before cache save",
        )
        self.assertEqual(
            purge_wut[0]["if"],
            "always()",
            "windows-unit-tests.yml whisper purge step must run if: always()",
        )

        purge_script_wut = "\n".join(purge_wut[0]["raw_lines"])
        self.assertIn("-ErrorAction Stop", purge_script_wut, "windows-unit-tests whisper purge must fail on removal error")
        self.assertIn("throw", purge_script_wut, "windows-unit-tests whisper purge must throw if directory still exists")

        test_step_idx = next(
            (i for i, s in enumerate(steps_wut) if "cross-platform assertions under msvc" in s["name"].lower()),
            None,
        )
        purge_step_idx = next(
            (i for i, s in enumerate(steps_wut) if "purge transient whisper-rs-sys" in s["name"].lower()),
            None,
        )
        self.assertIsNotNone(test_step_idx, "Could not find MSVC unit tests step in windows-unit-tests.yml")
        self.assertGreater(
            purge_step_idx,
            test_step_idx,
            "whisper purge must execute after MSVC unit tests in windows-unit-tests.yml",
        )

        # 2. build.yml
        content_build = (WORKFLOWS_DIR / "build.yml").read_text(encoding="utf-8")
        steps_build = parse_steps(content_build)

        purge_build = [s for s in steps_build if "purge transient whisper-rs-sys" in s["name"].lower()]
        self.assertEqual(
            len(purge_build),
            1,
            "build.yml must have exactly 1 whisper purge step for Windows before cache save",
        )
        self.assertEqual(
            purge_build[0]["if"],
            "matrix.target == 'windows' && always()",
            "build.yml whisper purge step must run if: matrix.target == 'windows' && always()",
        )

        purge_script_build = "\n".join(purge_build[0]["raw_lines"])
        self.assertIn("-ErrorAction Stop", purge_script_build, "build.yml whisper purge must fail on removal error")
        self.assertIn("throw", purge_script_build, "build.yml whisper purge must throw if directory still exists")

        build_step_idx = next(
            (i for i, s in enumerate(steps_build) if "build tauri app" in s["name"].lower()),
            None,
        )
        purge_build_idx = next(
            (i for i, s in enumerate(steps_build) if "purge transient whisper-rs-sys" in s["name"].lower()),
            None,
        )
        self.assertIsNotNone(build_step_idx, "Could not find Tauri build step in build.yml")
        self.assertGreater(
            purge_build_idx,
            build_step_idx,
            "whisper purge must execute after Tauri build in build.yml",
        )


if __name__ == "__main__":
    unittest.main()
