#!/usr/bin/env python3
"""Deterministic CPA build-output regressions."""

import importlib.util
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location(
    "build_cpa", Path(__file__).with_name("build-cpa.py")
)
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)


class BuildOutputTests(unittest.TestCase):
    def test_generated_files_have_distribution_safe_modes_under_shared_umask(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "out/cliproxyapi"
            fake_patch = root / "local.patch"
            fake_patch.write_text("")
            pin = {
                "repository": "https://example.invalid/cpa",
                "commit": "a" * 40,
                "version": "fixture-1",
                "built_at": "2026-09-21T00:00:00Z",
                "patch": fake_patch.name,
                "patch_sha256": "0" * 64,
            }

            def fake_run(command, cwd=None, **_kwargs):
                if command[0] == "tar":
                    (Path(command[command.index("-C") + 1]) / "LICENSE").write_text(
                        "fixture license\n"
                    )
                elif command[0] == "go":
                    Path(command[command.index("-o") + 1]).write_bytes(b"binary")
                return subprocess.CompletedProcess(command, 0)

            def fake_check_output(command, **kwargs):
                if command[0] == "git":
                    return b"fixture archive"
                self.assertEqual(command[:2], ["go", "version"])
                return "cliproxyapi: go1.fixture\n" if kwargs.get("text") else b""

            previous_umask = os.umask(0o002)
            try:
                with patch.object(builder, "pinned_source", return_value=(pin, fake_patch)), \
                        patch.object(builder.subprocess, "run", side_effect=fake_run), \
                        patch.object(
                            builder.subprocess,
                            "check_output",
                            side_effect=fake_check_output,
                        ):
                    builder.build(root / "source", "x86_64-unknown-linux-gnu", output)
            finally:
                os.umask(previous_umask)

            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o755)
            self.assertEqual(
                stat.S_IMODE(output.with_suffix(".LICENSE").stat().st_mode), 0o644
            )
            self.assertEqual(
                stat.S_IMODE(output.with_suffix(".provenance.json").stat().st_mode),
                0o644,
            )


if __name__ == "__main__":
    unittest.main()
