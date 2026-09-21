#!/usr/bin/env python3
"""Regression tests for baseline-aware Rust source-size enforcement."""

from pathlib import Path
import subprocess
import tempfile
import unittest

CHECK = Path(__file__).with_name("check-rust-file-size.sh").resolve()


def run(*command, cwd, check=True):
    return subprocess.run(
        command,
        cwd=cwd,
        check=check,
        text=True,
        capture_output=True,
    )


def write_lines(path, count):
    path.write_text("// line\n" * count)


class RustFileSizeTests(unittest.TestCase):
    def repository(self, initial_lines):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        repository = Path(temporary.name)
        run("git", "init", "-q", cwd=repository)
        run("git", "config", "user.name", "test", cwd=repository)
        run("git", "config", "user.email", "test@example.invalid", cwd=repository)
        write_lines(repository / "legacy.rs", initial_lines)
        run("git", "add", "legacy.rs", cwd=repository)
        run("git", "commit", "-qm", "base", cwd=repository)
        base = run("git", "rev-parse", "HEAD", cwd=repository).stdout.strip()
        return repository, base

    def commit_lines(self, repository, count):
        write_lines(repository / "legacy.rs", count)
        run("git", "add", "legacy.rs", cwd=repository)
        run("git", "commit", "-qm", "candidate", cwd=repository)

    def test_existing_hard_limit_violation_may_shrink_without_blocking(self):
        repository, base = self.repository(1200)
        self.commit_lines(repository, 1150)
        result = run("bash", str(CHECK), "--base", base, cwd=repository)
        self.assertIn("retains baseline size", result.stderr)

    def test_existing_hard_limit_violation_cannot_grow(self):
        repository, base = self.repository(1200)
        self.commit_lines(repository, 1201)
        result = run(
            "bash", str(CHECK), "--base", base, cwd=repository, check=False
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("error: legacy.rs has 1201 lines", result.stderr)

    def test_new_hard_limit_violation_is_rejected(self):
        repository, base = self.repository(1)
        write_lines(repository / "new.rs", 1101)
        result = run(
            "bash", str(CHECK), "--base", base, cwd=repository, check=False
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("error: new.rs has 1101 lines", result.stderr)


if __name__ == "__main__":
    unittest.main()
