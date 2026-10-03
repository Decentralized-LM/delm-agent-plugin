"""Release identity and provenance tests; never sign, publish, or call a model."""

import contextlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

import package_release
import publish_release
import qualify_release
import release_identity
from install_support import fingerprint, package_files
from test_installation import executable, fixture_source


@contextlib.contextmanager
def fixture():
    with tempfile.TemporaryDirectory(prefix="delm-release-test-") as temporary:
        root = Path(temporary)
        source = fixture_source(root / "source")
        subprocess.run(["git", "init", "-q", "-b", "main", str(source)], check=True)
        subprocess.run(["git", "-C", str(source), "add", "."], check=True)
        subprocess.run(["git", "-C", str(source), "-c", "user.name=Fixture", "-c",
                        "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false",
                        "-c", "core.hooksPath=/dev/null", "commit", "-qm", "Source"], check=True)
        revision = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
        runtime = root / "delm"
        executable(runtime, "#!/bin/sh\necho 'delm " + package_release.source_version(source) + "'\n")
        real_run = subprocess.run

        def run(arguments, **kwargs):
            if arguments[0] == "lipo":
                if "-thin" in arguments:
                    shutil.copy2(arguments[1], arguments[-1])
                return subprocess.CompletedProcess(arguments, 0)
            return real_run(arguments, **kwargs)

        with mock.patch.object(package_release.subprocess, "run", side_effect=run):
            yield root, source, runtime, revision


def qualifications(root, source, runtime, revision):
    result = []
    for architecture, target in package_release.ARCHITECTURES.items():
        path = root / (architecture + ".json")
        package_release.write_json(path, {
            "schema": 1, "kind": "native-release-build", "architecture": architecture,
            "target": target, "sourceRevision": revision, **package_release.source_state(source),
            "runtimeSha256": fingerprint(runtime)["sha256"], "passed": True,
            "modelCalls": 0, "lifecycleCases": package_release.LIFECYCLE_CASES,
        })
        result.append(path)
    return result


def checksums(output):
    (output / "SHA256SUMS").write_text("".join(
        entry["sha256"] + "  " + name + "\n" for name, entry in package_files(output).items()
        if name != "SHA256SUMS"))


class ReleaseTests(unittest.TestCase):
    def test_untracked_installer_inputs_mark_source_dirty(self):
        with fixture() as (_, source, _, _):
            self.assertFalse(package_release.source_state(source)["sourceDirty"])
            installer = source / "packages/installer"
            installer.mkdir(parents=True)
            (installer / "release.json").write_text('{"repository":null}')
            self.assertTrue(package_release.source_state(source)["sourceDirty"])

    def test_release_destination_is_explicit_and_publication_requires_matching_repository(self):
        with fixture() as (_, source, _, revision), mock.patch.object(release_identity, "SOURCE", source):
            before = Path.cwd()
            try:
                os.chdir(source)
                environment = {"SOURCE_REF": "main", "RELEASE_PUBLISH": "false",
                               "RELEASE_REPOSITORY": "example/distribution", "GITHUB_ACTIONS": "true",
                               "GITHUB_REPOSITORY": "example/source"}
                with mock.patch.dict(os.environ, environment, clear=True):
                    self.assertEqual(release_identity.main(), revision)
                    with mock.patch.dict(os.environ, RELEASE_PUBLISH="true"):
                        with self.assertRaisesRegex(SystemExit, "workflow's repository"):
                            release_identity.main()
                    with mock.patch.dict(os.environ, RELEASE_REPOSITORY="example/repo\nother=value"):
                        with self.assertRaisesRegex(SystemExit, "OWNER/REPO"):
                            release_identity.main()
            finally:
                os.chdir(before)

    def test_publisher_rejects_different_or_multiple_push_destinations(self):
        with mock.patch.object(publish_release, "git", side_effect=[
                "https://github.com/Example/DeLM.git", "git@github.com:example/delm.git"]):
            publish_release.verify_destination("example/delm")
        for push in ["https://github.com/example/other.git",
                     "https://github.com/example/delm.git\nhttps://github.com/example/other.git"]:
            with self.subTest(push=push), mock.patch.object(publish_release, "git", side_effect=[
                    "https://github.com/example/delm.git", push]):
                with self.assertRaisesRegex(SystemExit, "different destination"):
                    publish_release.verify_destination("example/delm")

    def test_signing_preflight_lists_missing_settings_without_importing_a_certificate(self):
        result = subprocess.run(["/bin/bash", str(package_release.SOURCE / "scripts/sign_release.sh")],
                                env={"PATH": "/usr/bin:/bin"}, text=True, capture_output=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("Signing is not configured", result.stderr)
        for name in ["APPLE_CERTIFICATE_BASE64", "APPLE_SIGNING_IDENTITY", "APPLE_TEAM_ID",
                     "APPLE_APP_PASSWORD", "RELEASE_REPOSITORY", "RELEASE_SOURCE_SHA"]:
            self.assertIn(name, result.stderr)
        self.assertIn("Private unsigned preparation", result.stderr)

    def test_resealed_extra_files_cannot_enter_the_release_distribution(self):
        for name in ["plugins/delm/auth.json", "private-validation.json"]:
            with self.subTest(name=name), fixture() as (root, source, runtime, revision):
                output = root / "review"
                package_release.assemble(source, runtime, output, "example/delm", revision)
                (output / name).write_text("must not be distributed")
                metadata = json.loads((output / "release.json").read_text())
                metadata["files"] = package_files(output / "plugins/delm")
                package_release.write_json(output / "release.json", metadata)
                checksums(output)
                with self.assertRaisesRegex(RuntimeError, "allowlist|unexpected artifacts"):
                    package_release.verify(output)

    def test_publisher_enforces_signed_evidence_before_any_remote_operation(self):
        with fixture() as (root, source, runtime, revision):
            records = qualifications(root, source, runtime, revision)
            origin, signed = root / "unsigned", root / "signed"
            package_release.assemble(source, runtime, origin, "example/delm", revision,
                                     qualifications=records)
            package_release.assemble(source, runtime, signed, "example/delm", revision,
                                     signed=True, unsigned_origin=origin)
            with mock.patch.dict(os.environ, {"GITHUB_ACTIONS": "true", "RELEASE_SOURCE_SHA": revision,
                                             "RELEASE_REPOSITORY": "example/delm"}, clear=True), \
                 mock.patch.object(publish_release, "verify_identity", return_value=revision), \
                 mock.patch.object(publish_release.sys, "argv", ["publish_release.py", str(signed), str(root / "missing")]), \
                 mock.patch.object(publish_release, "git") as remote:
                with self.assertRaises(FileNotFoundError):
                    publish_release.main()
                remote.assert_not_called()

    def test_signed_payload_must_match_qualified_unsigned_payload(self):
        with fixture() as (root, source, runtime, revision):
            records = qualifications(root, source, runtime, revision)
            origin = root / "unsigned"
            package_release.assemble(source, runtime, origin, "example/delm", revision,
                                     qualifications=records)
            skill = origin / "plugins/delm/skills/run/SKILL.md"
            skill.write_text(skill.read_text() + "\nAltered payload\n")
            metadata = json.loads((origin / "release.json").read_text())
            metadata["files"] = package_files(origin / "plugins/delm")
            package_release.write_json(origin / "release.json", metadata)
            checksums(origin)
            with self.assertRaisesRegex(RuntimeError, "Signed plugin payload differs"):
                package_release.assemble(source, runtime, root / "signed", "example/delm", revision,
                                         signed=True, unsigned_origin=origin)

    def test_unsigned_review_accepts_clean_branch_without_a_tag(self):
        with fixture() as (_, source, _, revision), mock.patch.object(release_identity, "SOURCE", source):
            before = Path.cwd()
            try:
                os.chdir(source)
                with mock.patch.dict(os.environ, {"SOURCE_REF": "main", "RELEASE_PUBLISH": "false"}, clear=True):
                    self.assertEqual(release_identity.main(), revision)
                with mock.patch.dict(os.environ, {"SOURCE_REF": "main", "RELEASE_PUBLISH": "true"}, clear=True):
                    with self.assertRaisesRegex(SystemExit, "requires the source tag"):
                        release_identity.main()
                tag = "v" + package_release.source_version(source)
                subprocess.run(["git", "tag", tag], check=True)
                with mock.patch.dict(os.environ, {"SOURCE_REF": tag, "RELEASE_PUBLISH": "true",
                                                 "RELEASE_SOURCE_SHA": revision}, clear=True):
                    self.assertEqual(release_identity.main(), revision)
                    (source / "src/main.rs").write_text("uncommitted source")
                    with self.assertRaisesRegex(SystemExit, "clean committed source"):
                        release_identity.main()
            finally:
                os.chdir(before)

    def test_dirty_unsigned_artifact_is_explicit_and_cannot_qualify(self):
        with fixture() as (root, source, runtime, revision):
            (source / "src/main.rs").write_text("working-tree source")
            output = root / "review"
            package_release.assemble(source, runtime, output, "example/delm", revision)
            metadata = package_release.verify(output, revision=revision, repository="example/delm")
            self.assertTrue(metadata["sourceDirty"])
            self.assertIn("uncommitted working tree", (output / "README.md").read_text())
            with self.assertRaisesRegex(RuntimeError, "working-tree artifact"):
                package_release.verify(output, require_qualified=True)

    def test_qualified_slices_must_match_exact_tested_runtime(self):
        with fixture() as (root, source, runtime, revision):
            records = qualifications(root, source, runtime, revision)
            package_release.assemble(source, runtime, root / "review", "example/delm", revision,
                                     qualifications=records)
            package_release.verify(root / "review", require_qualified=True)
            record = json.loads(records[1].read_text())
            record["runtimeSha256"] = "0" * 64
            package_release.write_json(records[1], record)
            with self.assertRaisesRegex(RuntimeError, "differs from the tested binary"):
                package_release.assemble(source, runtime, root / "mismatch", "example/delm", revision,
                                         qualifications=records)

    def test_catalog_and_provenance_are_validated_beyond_checksums(self):
        with fixture() as (root, source, runtime, revision):
            output = root / "review"
            package_release.assemble(source, runtime, output, "example/delm", revision)
            with self.assertRaisesRegex(RuntimeError, "source revision"):
                package_release.verify(output, revision="b" * 40)
            with self.assertRaisesRegex(RuntimeError, "repository"):
                package_release.verify(output, repository="unexpected/repository")
            catalog = output / ".agents/plugins/marketplace.json"
            value = json.loads(catalog.read_text())
            value["plugins"][0]["source"]["ref"] = "main"
            package_release.write_json(catalog, value)
            checksums(output)
            with self.assertRaisesRegex(RuntimeError, "immutable release identity"):
                package_release.verify(output)

    def test_missing_native_architecture_cannot_qualify(self):
        with fixture() as (root, source, runtime, revision):
            records = qualifications(root, source, runtime, revision)
            with self.assertRaisesRegex(RuntimeError, "Both native"):
                package_release.assemble(source, runtime, root / "review", "example/delm", revision,
                                         qualifications=records[:1])

    def test_rosetta_is_not_native_intel_qualification(self):
        with mock.patch.object(qualify_release.sys, "platform", "darwin"), \
             mock.patch.object(qualify_release.platform, "machine", return_value="x86_64"), \
             mock.patch.object(qualify_release.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "1\n", "")):
            with self.assertRaisesRegex(RuntimeError, "not emulation"):
                qualify_release.native_architecture("x86_64")

    def test_signed_reports_must_identify_the_published_binary(self):
        with fixture() as (root, source, runtime, revision):
            records = qualifications(root, source, runtime, revision)
            origin, signed = root / "unsigned", root / "signed"
            package_release.assemble(source, runtime, origin, "example/delm", revision,
                                     qualifications=records)
            package_release.assemble(source, runtime, signed, "example/delm", revision,
                                     signed=True, unsigned_origin=origin)
            reports = root / "reports"
            for architecture in package_release.ARCHITECTURES:
                package_release.write_json(reports / ("signed-qualification-" + architecture) / "result.json", {
                    "kind": "release-runtime-smoke", "architecture": architecture,
                    "runtimeSha256": fingerprint(runtime)["sha256"], "passed": True,
                    "signedAndNotarized": True, "modelCalls": 0, **package_release.source_state(source),
                    "cases": [{"case": case, "passed": True, "workerCount": 2,
                               "originalUnchanged": True} for case in ["complete", "stop"]]})
            with mock.patch.object(qualify_release, "command"):
                self.assertTrue(qualify_release.verify_signed(signed, reports)["passed"])
                path = reports / "signed-qualification-x86_64/result.json"
                report = json.loads(path.read_text())
                report["runtimeSha256"] = "0" * 64
                package_release.write_json(path, report)
                with self.assertRaisesRegex(RuntimeError, "does not match"):
                    qualify_release.verify_signed(signed, reports)


if __name__ == "__main__":
    unittest.main()
