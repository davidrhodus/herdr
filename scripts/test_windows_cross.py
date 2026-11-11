import unittest
from unittest.mock import patch
from scripts import windows_cross


class WindowsCrossTests(unittest.TestCase):
    def test_setup_installs_only_the_rust_target(self):
        with patch.object(windows_cross.subprocess, "run") as run:
            windows_cross.setup()
            run.assert_called_once_with(["rustup", "target", "add", windows_cross.TARGET], check=True)

    def test_lint_checks_windows_tests_without_linking(self):
        with patch.object(windows_cross.subprocess, "run") as run:
            windows_cross.lint()
            self.assertEqual(run.call_count, 2)
            command = run.call_args.args[0]
            self.assertEqual(command[:2], ["cargo", "clippy"])
            self.assertIn("--tests", command)
            self.assertIn(windows_cross.TARGET, command)
            self.assertNotIn("env", run.call_args.kwargs)
