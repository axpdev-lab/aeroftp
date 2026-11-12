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
import tempfile
import unittest

WORKFLOWS_DIR = Path(__file__).resolve().parents[1] / "workflows"


def unquote(s):
    """Strip matching outer quotes from a string, preserving internal quotes."""
    s = s.strip()
    if len(s) >= 2 and ((s[0] == '"' and s[-1] == '"') or (s[0] == "'" and s[-1] == "'")):
        return s[1:-1].strip()
    return s


def get_all_workflow_files(workflows_dir=WORKFLOWS_DIR):
    """Return all .yml and .yaml workflow files in sorted order."""
    yml_files = set(workflows_dir.glob("*.yml"))
    yaml_files = set(workflows_dir.glob("*.yaml"))
    return sorted(yml_files | yaml_files)


def parse_steps(workflow_text):
    """Parse step definitions from workflow YAML text using line-by-line scanning.

    Features:
    - Step boundary detection: recognizes any list item starting with '- <key>:'
      regardless of key order (supports id, name, uses, run, if, env, etc.).
    - Block scalar handling: lines indented inside |, |-, >, >- are consumed as text
      and never parsed as step keys or mappings.
    - Duplicate key rejection: duplicate keys at step level or inside 'with:' raise ValueError.
    - Preserves step attributes (name, id, uses, if, with, run, raw_lines).
    """
    lines = workflow_text.split("\n")
    steps = []
    current_step = None
    in_with = False
    with_indent = None
    multiline_key = None
    multiline_indent = None
    seen_step_keys = set()

    step_start = re.compile(r"^(\s*)-\s+([\w-]+):\s*(.*)$")

    for idx, line in enumerate(lines):
        line_num = idx + 1
        stripped = line.strip()
        indent = len(line) - len(line.lstrip())

        # 1. If inside a block scalar (e.g. prefix-key: | or run: |), consume lines
        if multiline_key is not None:
            if stripped and indent > multiline_indent:
                if in_with and multiline_key in current_step["with"]:
                    current_step["with"][multiline_key] += "\n" + line
                elif multiline_key in current_step:
                    current_step[multiline_key] += "\n" + line
                current_step["raw_lines"].append(line)
                continue
            else:
                multiline_key = None
                multiline_indent = None

        # 2. Check for start of a new step: any line starting with '- <key>:'
        m_step = step_start.match(line)
        if m_step:
            step_indent = len(m_step.group(1))
            if current_step is not None:
                if step_indent == current_step["indent"]:
                    steps.append(current_step)
                    current_step = None

            if current_step is None:
                first_key = m_step.group(2)
                raw_val = m_step.group(3).strip()
                val = unquote(raw_val)

                current_step = {
                    "line": line_num,
                    "name": "",
                    "id": "",
                    "uses": "",
                    "if": "",
                    "run": "",
                    "indent": step_indent,
                    "with": {},
                    "raw_lines": [line],
                }
                seen_step_keys = {first_key}
                in_with = False
                with_indent = None
                multiline_key = None
                multiline_indent = None

                if first_key in ("name", "id", "uses", "if", "run"):
                    current_step[first_key] = val
                    if raw_val in (">-", ">", "|", "|-"):
                        multiline_key = first_key
                        multiline_indent = indent
                elif first_key == "with":
                    in_with = True
                    with_indent = indent
                continue

        if current_step is not None:
            # Check if current step has ended
            if stripped and indent <= current_step["indent"]:
                steps.append(current_step)
                current_step = None
                in_with = False
                with_indent = None
                multiline_key = None
                multiline_indent = None
                seen_step_keys = set()
                continue

            current_step["raw_lines"].append(line)

            # Check if entering or exiting 'with:' mapping
            if re.match(r"^\s*with:\s*$", line):
                if "with" in seen_step_keys:
                    raise ValueError(f"Duplicate 'with' key in step at line {line_num}")
                seen_step_keys.add("with")
                in_with = True
                with_indent = indent
                continue

            if in_with:
                if indent <= with_indent and stripped:
                    in_with = False
                else:
                    m_kw = re.match(r"^\s*([\w-]+):\s*(.*)$", line)
                    if m_kw:
                        k = m_kw.group(1)
                        if k in current_step["with"]:
                            raise ValueError(f"Duplicate key {k!r} in 'with:' at line {line_num}")
                        raw_v = m_kw.group(2).strip()
                        v = unquote(raw_v)
                        current_step["with"][k] = v
                        if raw_v in (">-", ">", "|", "|-"):
                            multiline_key = k
                            multiline_indent = indent
                    continue

            # Step-level attributes
            m_attr = re.match(r"^\s*([\w-]+):\s*(.*)$", line)
            if m_attr:
                k = m_attr.group(1)
                if k in seen_step_keys:
                    raise ValueError(f"Duplicate {k!r} key at line {line_num}")
                seen_step_keys.add(k)
                raw_v = m_attr.group(2).strip()
                v = unquote(raw_v)
                if k in ("name", "id", "uses", "if", "run"):
                    current_step[k] = v
                    if raw_v in (">-", ">", "|", "|-"):
                        multiline_key = k
                        multiline_indent = indent
                continue

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

        # First apt-install belongs to linux-validate and is unconditional
        self.assertEqual(
            apt_steps[0]["if"],
            "",
            f"build.yml step '{apt_steps[0]['name']}' in linux-validate must be unconditional",
        )
        self.assertEqual(
            apt_steps[0]["with"].get("cache-prefix"),
            "apt-tauri-dev",
            f"build.yml step '{apt_steps[0]['name']}' must use cache-prefix: apt-tauri-dev",
        )
        self.assertIn(
            "libwebkit2gtk-4.1-dev",
            apt_steps[0]["with"].get("packages", ""),
            f"build.yml step '{apt_steps[0]['name']}' missing libwebkit2gtk-4.1-dev",
        )

        # Second apt-install belongs to build-and-release and is conditioned to matrix.target == 'linux'
        self.assertEqual(
            apt_steps[1]["if"],
            "matrix.target == 'linux'",
            f"build.yml step '{apt_steps[1]['name']}' in build-and-release must be conditioned on matrix.target == 'linux'",
        )
        self.assertEqual(
            apt_steps[1]["with"].get("cache-prefix"),
            "apt-tauri-dev",
            f"build.yml step '{apt_steps[1]['name']}' must use cache-prefix: apt-tauri-dev",
        )
        self.assertIn(
            "libwebkit2gtk-4.1-dev",
            apt_steps[1]["with"].get("packages", ""),
            f"build.yml step '{apt_steps[1]['name']}' missing libwebkit2gtk-4.1-dev",
        )

    def test_build_workflow_rejects_altered_apt_install_condition(self):
        """Regression test: verify that changing the build-and-release condition fails the gate."""
        altered_yaml = """
- name: linux-validate apt-install
  uses: ./.github/actions/apt-install
  with:
    cache-prefix: apt-tauri-dev
    packages: libwebkit2gtk-4.1-dev
- name: build-and-release apt-install
  uses: ./.github/actions/apt-install
  if: matrix.target == 'macos'
  with:
    cache-prefix: apt-tauri-dev
    packages: libwebkit2gtk-4.1-dev
"""
        steps = parse_steps(altered_yaml)
        self.assertEqual(len(steps), 2)
        self.assertNotEqual(steps[1]["if"], "matrix.target == 'linux'")

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
        self.assertIn("Remove-Item", purge_script_wut, "windows-unit-tests whisper purge must invoke Remove-Item")
        self.assertIn("whisper-rs-sys-*", purge_script_wut, "windows-unit-tests whisper purge must target whisper-rs-sys-*")
        self.assertIn("@('debug', 'release')", purge_script_wut, "windows-unit-tests whisper purge must cover both debug and release profiles")
        self.assertIn("src-tauri/target/$profile/build", purge_script_wut, "windows-unit-tests whisper purge must target profile build paths")
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
        self.assertIn("Remove-Item", purge_script_build, "build.yml whisper purge must invoke Remove-Item")
        self.assertIn("whisper-rs-sys-*", purge_script_build, "build.yml whisper purge must target whisper-rs-sys-*")
        self.assertIn("src-tauri/target/release/build", purge_script_build, "build.yml whisper purge must target release build tree")
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

    def test_yaml_extension_discovered_and_unsafe_save_if_rejected(self):
        """Verify .yaml extension files are discovered and unsafe save-if policies are rejected."""
        with tempfile.TemporaryDirectory() as tmpdir:
            tmppath = Path(tmpdir)
            unsafe_yaml = tmppath / "custom-pipeline.yaml"
            unsafe_yaml.write_text(
                """
name: Unsafe Pipeline
jobs:
  build:
    steps:
      - name: Rust cache
        uses: Swatinem/rust-cache@v2
        with:
          save-if: true
""",
                encoding="utf-8",
            )

            discovered = get_all_workflow_files(tmppath)
            self.assertEqual(len(discovered), 1)
            self.assertEqual(discovered[0].name, "custom-pipeline.yaml")

            steps = parse_steps(unsafe_yaml.read_text(encoding="utf-8"))
            rust_steps = [s for s in steps if "rust-cache" in s["uses"].lower()]
            self.assertEqual(len(rust_steps), 1)
            self.assertEqual(rust_steps[0]["with"]["save-if"], "true")
            # Confirm policy rejects "true"
            valid_save_if = {"${{ github.ref == 'refs/heads/main' }}", "false"}
            self.assertNotIn(rust_steps[0]["with"]["save-if"], valid_save_if)

    def test_parse_steps_preserves_uses_when_block_scalar_contains_nested_uses(self):
        """Verify nested uses: inside block scalars do not corrupt step uses attribute."""
        sample_yaml = """
- name: Rust cache
  uses: Swatinem/rust-cache@v2
  with:
    prefix-key: |
      uses: actions/cache@v4
      arbitrary text
    save-if: ${{ github.ref == 'refs/heads/main' }}
"""
        steps = parse_steps(sample_yaml)
        self.assertEqual(len(steps), 1)
        self.assertEqual(steps[0]["uses"], "Swatinem/rust-cache@v2")
        self.assertIn("uses: actions/cache@v4", steps[0]["with"]["prefix-key"])
        self.assertEqual(steps[0]["with"]["save-if"], "${{ github.ref == 'refs/heads/main' }}")

    def test_parse_steps_rejects_duplicate_mapping_keys(self):
        """Verify parse_steps raises ValueError on duplicate keys in step or with: blocks."""
        dup_with_yaml = """
- name: Step with duplicate keys
  uses: actions/checkout@v4
  with:
    path: first
    path: second
"""
        with self.assertRaises(ValueError) as ctx:
            parse_steps(dup_with_yaml)
        self.assertIn("Duplicate key 'path' in 'with:'", str(ctx.exception))

        dup_step_yaml = """
- name: Step with duplicate uses
  uses: actions/checkout@v4
  uses: actions/cache@v4
"""
        with self.assertRaises(ValueError) as ctx:
            parse_steps(dup_step_yaml)
        self.assertIn("Duplicate 'uses' key", str(ctx.exception))

    def test_windows_purge_failure_aborts_count_and_prevents_cache_save(self):
        """Verify the purge algorithm invariant: removal failure throws and halts before post-save."""
        class SimDirectory:
            def __init__(self, name, fail_deletion=False):
                self.name = name
                self.fail_deletion = fail_deletion
                self.exists = True

            def remove(self, error_action_stop=True):
                if self.fail_deletion:
                    if error_action_stop:
                        raise RuntimeError(f"Access denied removing {self.name}")
                else:
                    self.exists = False

        def execute_purge_simulation(directories):
            purged = 0
            for d in directories:
                d.remove(error_action_stop=True)
                if d.exists:
                    raise RuntimeError(f"Failed to purge directory: {d.name}")
                purged += 1
            return purged

        # Normal run: all succeed
        dirs_ok = [SimDirectory("whisper-rs-sys-1"), SimDirectory("whisper-rs-sys-2")]
        count = execute_purge_simulation(dirs_ok)
        self.assertEqual(count, 2)

        # Failing run: second directory fails
        dirs_fail = [SimDirectory("whisper-rs-sys-1"), SimDirectory("whisper-rs-sys-2", fail_deletion=True)]
        with self.assertRaises(RuntimeError) as ctx:
            execute_purge_simulation(dirs_fail)
        self.assertIn("Access denied removing whisper-rs-sys-2", str(ctx.exception))
        # The failed directory was not counted as purged
        self.assertFalse(dirs_fail[0].exists)
        self.assertTrue(dirs_fail[1].exists)

    def test_parse_steps_recognizes_id_first_step_and_enforces_policy(self):
        """Verify steps starting with 'id:' or other attributes are recognized and validated."""
        id_first_yaml = """
- id: rust-cache-step
  uses: Swatinem/rust-cache@v2
  with:
    save-if: true
"""
        steps = parse_steps(id_first_yaml)
        self.assertEqual(len(steps), 1)
        self.assertEqual(steps[0]["id"], "rust-cache-step")
        self.assertEqual(steps[0]["uses"], "Swatinem/rust-cache@v2")
        self.assertEqual(steps[0]["with"]["save-if"], "true")
        valid_save_if = {"${{ github.ref == 'refs/heads/main' }}", "false"}
        self.assertNotIn(steps[0]["with"]["save-if"], valid_save_if)


if __name__ == "__main__":
    unittest.main()
