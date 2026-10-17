"""Run the sign-off step of .github/workflows/dco.yml on a scratch repository.

The step is taken from the workflow file as it is, so these tests run the
check that pull requests get. The repository is real git with no stubs: a
base commit the clone does not have is the realistic way for git rev-list to
fail.

The step used to loop with `for sha in $(git rev-list ...)`. set -e does not
see a command substitution in a `for` word list, so a failing rev-list gave
an empty loop and the step printed "All commits are signed off.". Those
cases are below.
"""

from pathlib import Path
import os
import subprocess
import tempfile
import textwrap
import unittest


WORKFLOW = Path(__file__).resolve().parents[1] / 'workflows/dco.yml'
AUTHOR = ('Ada Example', 'ada@example.org')


def step_script():
    workflow = WORKFLOW.read_text()
    step = workflow.split('- name: Every commit must be signed off', 1)[1]
    step = step.split('\n      - name:', 1)[0]
    return textwrap.dedent(step.split('        run: |\n', 1)[1])


class DcoCheckTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name)
        self.env = dict(
            os.environ,
            GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM='1',
            GIT_AUTHOR_NAME=AUTHOR[0], GIT_AUTHOR_EMAIL=AUTHOR[1],
            GIT_COMMITTER_NAME=AUTHOR[0], GIT_COMMITTER_EMAIL=AUTHOR[1],
        )
        self.git('init', '-q', '-b', 'main')
        self.base = self.commit('base', signed=True)

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, *args):
        return subprocess.run(['git', *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout.strip()

    def commit(self, subject, *, signed):
        message = subject
        if signed:
            message += f'\n\nSigned-off-by: {AUTHOR[0]} <{AUTHOR[1]}>'
        self.git('commit', '-q', '--allow-empty', '-m', message)
        return self.git('rev-parse', 'HEAD')

    def check(self, base, head):
        script = self.repo / 'step.sh'
        script.write_text(step_script())
        return subprocess.run(['bash', '-e', str(script)], cwd=self.repo,
                              env=dict(self.env, BASE=base, HEAD=head),
                              capture_output=True, text=True, check=False)

    def test_signed_commits_pass_and_are_counted(self):
        self.commit('one', signed=True)
        head = self.commit('two', signed=True)
        result = self.check(self.base, head)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('All commits are signed off (2 checked).', result.stdout)

    def test_unsigned_commit_fails(self):
        self.commit('one', signed=True)
        head = self.commit('two', signed=False)
        result = self.check(self.base, head)
        self.assertEqual(result.returncode, 1)
        self.assertIn("is missing 'Signed-off-by: Ada Example <ada@example.org>'", result.stdout)

    def test_merge_commits_alone_are_exempt(self):
        # Two merges of the same two parents: the base is one of them, so
        # the range holds the other merge commit and nothing else.
        main_tip = self.commit('main side', signed=True)
        self.git('checkout', '-q', '-b', 'side', self.base)
        side_tip = self.commit('other side', signed=True)
        self.git('checkout', '-q', 'main')
        self.git('merge', '-q', '--no-ff', '-m', 'first merge', 'side')
        base = self.git('rev-parse', 'HEAD')
        self.git('checkout', '-q', '--detach', main_tip)
        self.git('merge', '-q', '--no-ff', '-m', 'second merge', side_tip)
        head = self.git('rev-parse', 'HEAD')
        result = self.check(base, head)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('Only merge commits in the range (1), which are exempt.', result.stdout)

    # The unsigned commit is in the range each time: a pass here means the
    # range was never read.
    def test_range_git_cannot_read_fails(self):
        head = self.commit('unsigned', signed=False)
        missing = '0123456789abcdef0123456789abcdef01234567'
        for name, base, tip in (
            ('base not in the clone', missing, head),
            ('head not in the clone', self.base, missing),
            ('base empty', '', head),
        ):
            with self.subTest(name):
                result = self.check(base, tip)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertNotIn('signed off.', result.stdout)

    def test_empty_range_fails(self):
        result = self.check(self.base, self.base)
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn('::error::no commits between', result.stdout)
        self.assertNotIn('signed off.', result.stdout)


if __name__ == '__main__':
    unittest.main()
