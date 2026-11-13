import tempfile
import os
from unittest import mock
import unittest
from pathlib import Path

from scripts import build_linux_bundle as bundle


class LinuxBundleTests(unittest.TestCase):
    def test_identity_captures_dirty_source_but_ignores_build_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("Cargo.toml", "Cargo.lock", "build.rs", "scripts/build_linux_bundle.py"):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("initial")
            (root / "src").mkdir()
            source = root / "src/main.rs"
            source.write_text("first")
            initial = bundle.source_id(root)
            self.assertEqual(initial, bundle.source_id(root))
            (root / "src/target").mkdir()
            (root / "src/target/output").write_text("generated")
            self.assertEqual(initial, bundle.source_id(root))
            source.write_text("dirty")
            self.assertNotEqual(initial, bundle.source_id(root))
            source.write_text("first")
            (root / "src/new.rs").write_text("untracked")
            self.assertNotEqual(initial, bundle.source_id(root))

    def test_builder_variants_and_checkout_locations_have_distinct_identities(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            for name in ("Cargo.toml","Cargo.lock","build.rs","scripts/build_linux_bundle.py"):
                path=root/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_text("source")
            with mock.patch.dict(os.environ,{"HERDR_BUILD_VARIANT":"first"}):
                first=bundle.source_id(root)
            with mock.patch.dict(os.environ,{"HERDR_BUILD_VARIANT":"second"}):
                self.assertNotEqual(first,bundle.source_id(root))

    def test_leaf_environment_cannot_embed_an_old_bundle_recursively(self):
        original = {
            "HERDR_LINUX_X86_64_BINARY": "/old/intel",
            "HERDR_LINUX_AARCH64_BINARY": "/old/arm",
            "HERDR_BUILD_CHANNEL": "preview",
            "HERDR_BUILD_ID": "old",
            "PATH": "/tools",
        }
        result = bundle.leaf_environment(original, "new")
        self.assertEqual(result["HERDR_BUILD_CHANNEL"], "local")
        self.assertEqual(result["HERDR_BUILD_ID"], "new")
        self.assertEqual(result["PATH"], "/tools")
        for variable in bundle.BUNDLE_VARIABLES:
            self.assertNotIn(variable, result)
            self.assertIn(variable, original)


if __name__ == "__main__":
    unittest.main()
