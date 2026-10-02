#!/usr/bin/env python3
"""What check-seeds.py says about a seed's boot title.

The title is the one piece of a seed's `sandbox.json` that no manifest
carries, so nothing downstream of the check would notice it missing: the
build would fail late, or two seeds would ship boot pages nobody can tell
apart. These run the check's own functions over copies of the real seeds.

Usage: python3 -m unittest discover -s examples/dev-sandbox/seeds -p 'test_*.py'
"""
import contextlib
import importlib.util
import io
import json
import pathlib
import shutil
import sys
import tempfile
import unittest

SEEDS = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(SEEDS))
import seedlib  # noqa: E402

_spec = importlib.util.spec_from_file_location("check_seeds", SEEDS / "check-seeds.py")
check_seeds = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(check_seeds)


def check(seeds: list) -> list:
    """`check_seeds.check` without its per-seed progress lines."""
    with contextlib.redirect_stderr(io.StringIO()):
        return check_seeds.check(seeds)


class BootTitle(unittest.TestCase):
    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)

    def copy(self, name: str, as_name: str = None) -> pathlib.Path:
        target = self.tmp / (as_name or name)
        shutil.copytree(SEEDS / name, target)
        return target

    def edit(self, seed: pathlib.Path, change) -> None:
        path = seed / "sandbox.json"
        sandbox = json.loads(path.read_text(encoding="utf-8"))
        change(sandbox)
        path.write_text(json.dumps(sandbox, indent=2) + "\n", encoding="utf-8")

    def test_the_committed_seeds_pass_and_each_names_itself(self):
        seeds = seedlib.seed_dirs()
        self.assertEqual(check(seeds), [])
        titles = [seedlib.boot_title(seed) for seed in seeds]
        self.assertNotIn(None, titles)
        self.assertEqual(len(set(titles)), len(titles), titles)

    def test_a_copy_of_a_committed_seed_passes(self):
        # The baseline for every refusal below: the copy itself is clean.
        self.assertEqual(check([self.copy("bootstrap")]), [])

    def test_a_seed_with_no_title_is_refused(self):
        seed = self.copy("bootstrap")
        self.edit(seed, lambda sandbox: sandbox.pop("title"))
        problems = check([seed])
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("sandbox.json", problems[0])
        self.assertIn("title", problems[0])

    def test_a_title_that_is_not_one_line_of_text_is_refused(self):
        for bad in ["", "  ", " padded", "padded ", "two\nlines", "tab\there", 7, None, "x" * 121]:
            with self.subTest(title=bad):
                seed = self.copy("bootstrap", f"case-{abs(hash(repr(bad)))}")
                self.edit(seed, lambda sandbox: sandbox.__setitem__("title", bad))
                problems = check([seed])
                self.assertEqual(len(problems), 1, problems)
                self.assertIn("title", problems[0])

    def test_two_seeds_with_one_title_are_refused(self):
        # What the sandboxes shipped with before each seed named itself: a
        # second seed made from the first, title and all.
        blank = self.copy("blank")
        bootstrap = self.copy("bootstrap")
        self.edit(bootstrap, lambda sandbox: sandbox.__setitem__("title", seedlib.boot_title(blank)))
        problems = check([blank, bootstrap])
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("blank, bootstrap: share the title", problems[0])

    def test_the_title_is_read_from_sandbox_json_and_nowhere_else(self):
        # A retitled seed is still a clean seed — no manifest or generated
        # file restates the title — and what the build stages follows it.
        seed = self.copy("bootstrap")
        self.edit(seed, lambda sandbox: sandbox.__setitem__("title", "Another title"))
        self.assertEqual(check([seed]), [])
        self.assertEqual(seedlib.boot_title(seed), "Another title")


if __name__ == "__main__":
    unittest.main()
