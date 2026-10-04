"""Dependency notice provenance tests without downloads, compilation, or model calls."""

import copy
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import build
import dependency_notices as notices


class DependencyNoticesTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="delm-license-test-")
        self.addCleanup(self.temporary.cleanup)
        self.source = Path(self.temporary.name)
        (self.source / "Cargo.toml").write_text('[package]\nname="delm"\n')
        (self.source / "Cargo.lock").write_text("# locked dependency fixture\n")
        (self.source / "rust-toolchain.toml").write_text('[toolchain]\nchannel="1.95.0"\n')
        self.packages = {}
        for name in ["delm", "shared", "intel", "build-tool", "dev-only"]:
            root = self.source / "registry" / name
            root.mkdir(parents=True)
            (root / "Cargo.toml").write_text(f'[package]\nname="{name}"\n')
            (root / "LICENSE-MIT").write_text("Permission and copyright fixture.\n")
            self.packages[name] = {"id": name, "name": name, "version": "1.0.0", "license": "MIT",
                                   "manifest_path": str(root / "Cargo.toml"), "source": "registry+fixture"}
        self.graph = {"packages": list(self.packages.values()), "resolve": {
            "root": "delm", "nodes": [{"id": name, "deps": []} for name in self.packages]}}
        self.graph["resolve"]["nodes"][0]["deps"] = [
            {"pkg": "shared", "dep_kinds": [{"kind": None}]},
            {"pkg": "build-tool", "dep_kinds": [{"kind": "build"}]},
            {"pkg": "dev-only", "dep_kinds": [{"kind": "dev"}]},
        ]

    def test_target_union_includes_runtime_and_build_not_dev_and_deduplicates_text(self):
        intel = copy.deepcopy(self.graph)
        intel["resolve"]["nodes"][0]["deps"].append({"pkg": "intel", "dep_kinds": [{"kind": None}]})
        crate = Path(self.packages["shared"]["manifest_path"]).parent
        (crate / "NOTICE").write_text("Shared upstream attribution.\n")
        result = notices.render(self.source, [self.graph, intel])
        for name in ["shared", "intel", "build-tool"]:
            self.assertIn(f"\n{name} 1.0.0\n", result)
        self.assertNotIn("\ndev-only 1.0.0\n", result)
        self.assertNotIn("\ndelm 1.0.0\n", result)
        self.assertEqual(result.count("Permission and copyright fixture."), 1)
        self.assertIn("Shared upstream attribution.", result)
        self.assertNotIn(str(self.source), result)

    def test_missing_empty_or_notice_only_license_fails(self):
        package = self.packages["shared"]
        root = Path(package["manifest_path"]).parent
        (root / "LICENSE-MIT").write_text("")
        with self.assertRaisesRegex(RuntimeError, "empty"):
            notices.license_texts(package)
        (root / "LICENSE-MIT").unlink()
        (root / "NOTICE").write_text("Attribution without a license grant.")
        with self.assertRaisesRegex(RuntimeError, "no license text"):
            notices.license_texts(package)

    def test_custom_license_file_is_required_and_preserved(self):
        package = dict(self.packages["shared"], license=None, license_file="terms/custom.txt")
        root = Path(package["manifest_path"]).parent
        with self.assertRaises(OSError):
            notices.license_texts(package)
        (root / "terms").mkdir()
        (root / "terms/custom.txt").write_text("Custom license text.\n")
        self.assertIn(("terms/custom.txt", "Custom license text.\n"), notices.license_texts(package))

    def test_linked_or_external_license_is_rejected(self):
        package = self.packages["shared"]
        root = Path(package["manifest_path"]).parent
        outside = self.source / "outside-license"
        outside.write_text("External text\n")
        (root / "LICENSE-MIT").unlink()
        (root / "LICENSE-MIT").symlink_to(outside)
        with self.assertRaisesRegex(RuntimeError, "linked or external"):
            notices.license_texts(package)
        (root / "LICENSE-MIT").unlink()
        with self.assertRaisesRegex(RuntimeError, "linked or external"):
            notices.license_texts(dict(package, license_file=str(outside)))

    def test_packaging_rejects_stale_manifest_or_lock_without_creating_output(self):
        for name in notices.INPUTS:
            with self.subTest(name=name):
                original = (self.source / name).read_text()
                (self.source / notices.FILENAME).write_text(notices.render(self.source, [self.graph]))
                self.assertEqual(notices.validate(self.source), self.source / notices.FILENAME)
                (self.source / name).write_text(original + "# changed\n")
                with self.assertRaisesRegex(RuntimeError, "stale"):
                    build.stage_package(self.source, self.source / "unused", self.source / "package")
                self.assertFalse((self.source / "package").exists())
                (self.source / name).write_text(original)

    def test_current_bundles_require_notices_and_owned_license_is_mit(self):
        notices.validate(notices.SOURCE)
        for resources in build.HOST_PACKAGE_FILES.values():
            self.assertIn(notices.FILENAME, resources)
        self.assertEqual((notices.SOURCE / "LICENSE").read_text(),
                         (notices.SOURCE / "packages/installer/LICENSE").read_text())
        self.assertTrue((notices.SOURCE / "LICENSE").read_text().startswith("MIT License\n"))
        self.assertIn("Rust standard library (rustc", (notices.SOURCE / notices.FILENAME).read_text())

    def test_standard_library_inventory_preserves_license_text_and_links(self):
        parser = notices.LicenseHTML()
        parser.feed('<h1>Rust Standard Library</h1><p>Copyright &amp; permission</p>'
                    '<pre>Permission is hereby granted\nFull license terms.</pre>'
                    '<a href="https://example.invalid/license">License source</a>')
        text = "".join(parser.parts)
        self.assertIn("Copyright & permission", text)
        self.assertIn("Permission is hereby granted\nFull license terms.", text)
        self.assertIn("https://example.invalid/license", text)

    def test_missing_standard_library_notices_or_wrong_toolchain_fail(self):
        with mock.patch.object(notices.subprocess, "check_output", side_effect=["rustc 1.95.0 (fixture)\n", str(self.source)]):
            with self.assertRaises(OSError):
                notices.standard_library_notices(self.source)
        with mock.patch.object(notices.subprocess, "check_output", return_value="rustc 1.94.0 (fixture)\n"):
            with self.assertRaisesRegex(RuntimeError, "pinned Rust toolchain"):
                notices.standard_library_notices(self.source)


if __name__ == "__main__":
    unittest.main()
