import json
from pathlib import Path
import tempfile
import unittest

from scripts.fuzz_regressions import preserve


class FuzzRegressionTests(unittest.TestCase):
    def test_failures_are_byte_exact_deduplicated_and_replayable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifacts = root / "artifacts"
            artifacts.mkdir()
            data = b"\x00\xff\xc2\x93\r\n\x1b[31m"
            for name in ["crash-one", "timeout-two", "oom-three", "leak-four"]:
                (artifacts / name).write_bytes(data)
            (artifacts / "slow-unit-not-a-failure").write_bytes(b"ignore")
            archive = root / "download"
            for target in ["parser_chunking", "parser_operations"]:
                names = preserve(target, artifacts, root, archive)
                self.assertEqual(len(names), 4)
                self.assertEqual(len(set(names)), 1)
                seed = Path("fuzz/corpus") / target / names[0]
                self.assertTrue(seed.name.startswith("seed-regression-"))
                self.assertEqual((root / seed).read_bytes(), data)
                self.assertEqual((archive / seed).read_bytes(), data)
                self.assertEqual(names, preserve(target, artifacts, root, archive))
                manifest = json.loads((archive / "regressions.json").read_text())
                self.assertEqual(manifest["target"], target)
                self.assertEqual(len(manifest["failures"]), 4)

    def test_missing_artifacts_still_archives_cached_regressions(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            seed = root / "fuzz/corpus/parser_chunking/seed-regression-cached"
            seed.parent.mkdir(parents=True)
            seed.write_bytes(b"cached\x00")
            archive = root / "download"
            self.assertEqual(preserve("parser_chunking", root / "missing", root, archive), [])
            self.assertEqual((archive / seed.relative_to(root)).read_bytes(), seed.read_bytes())
            self.assertEqual(json.loads((archive / "regressions.json").read_text())["failures"], [])

    def test_unknown_target_is_rejected_before_writing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaises(ValueError):
                preserve("../escape", root, root)
            self.assertEqual(list(root.iterdir()), [])
