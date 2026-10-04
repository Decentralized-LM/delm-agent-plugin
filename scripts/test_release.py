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
import build
import publish_release
import qualify_release
import release_identity
from install_support import fingerprint, package_files
from test_installation import executable, fixture_claude_qualifications, fixture_claude_validation, fixture_runtime, fixture_source


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
        fixture_runtime(runtime, package_release.source_version(source))
        real_run = subprocess.run

        def run(arguments, **kwargs):
            if arguments[0] == "lipo":
                if "-thin" in arguments:
                    shutil.copy2(arguments[1], arguments[-1])
                return subprocess.CompletedProcess(arguments, 0)
            return real_run(arguments, **kwargs)

        with mock.patch.object(package_release.subprocess, "run", side_effect=run), \
             mock.patch.object(package_release, "validate_claude_package", side_effect=fixture_claude_validation):
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
            "codexVersion": "fixture 1", "exactLiveSessionParity": False,
            "evidenceSha256": {name: "a" * 64 for name in ["smoke", "inheritance", *package_release.LIFECYCLE_CASES]},
            "nativeInheritance": {
                "runtimeSha256": fingerprint(runtime)["sha256"], "architecture": architecture,
                "hostVersion": "fixture 1", "gatewayToolCalled": True,
                "parentCliOverridesNotExported": True, "exactLiveSessionParity": False,
                "nativeTestSha256": "b" * 64, "sourceDigest": "c" * 64,
            },
        })
        result.append(path)
    return result


def checksums(output):
    (output / "SHA256SUMS").write_text("".join(
        entry["sha256"] + "  " + name + "\n" for name, entry in package_files(output).items()
        if name != "SHA256SUMS"))


class ReleaseTests(unittest.TestCase):
    def test_adapter_provenance_ignores_native_editor_generation_but_binds_executed_resources(self):
        with fixture() as (root, source, runtime, revision):
            package = root / "claude"
            build.stage_package(source, runtime, package, "claude")
            before = build.claude_adapter_digest(package)
            generated = package / ".claude-plugin/types/claude-code"
            generated.mkdir(parents=True)
            (generated / "index.d.ts").write_text("// Native-generated editor types\n")
            (package / ".claude-plugin/types/tsconfig.json").write_text('{"compilerOptions":{}}')
            (package / "tsconfig.json").write_text('{"extends":"./.claude-plugin/types/tsconfig.json"}')
            self.assertEqual(build.claude_adapter_digest(package), before)
            manifest = package / ".claude-plugin/plugin.json"
            data = json.loads(manifest.read_text())
            data["repository"] = "https://github.com/example/delm"
            manifest.write_text(json.dumps(data))
            self.assertEqual(build.claude_adapter_digest(package), before)
            for name in ("delm.js", "board-view.js", "board-render.js"):
                with self.subTest(module=name):
                    module = package / "hooks" / name
                    original = module.read_text()
                    module.write_text(original + "\n// changed executed adapter")
                    self.assertNotEqual(build.claude_adapter_digest(package), before)
                    module.unlink()
                    with self.assertRaisesRegex(RuntimeError, "adapter resources are missing"):
                        build.claude_adapter_digest(package)
                    module.write_text(original)

    def test_claude_payload_is_required_self_contained_and_bound_to_native_validation(self):
        with fixture() as (root, source, runtime, revision):
            output = root / "review"
            package_release.assemble(source, runtime, output, "example/delm", revision)
            metadata = package_release.verify(output)
            self.assertEqual(metadata["claudeValidation"]["payloadSha256"],
                             build.payload_digest(output / "plugins/delm-claude"))
            self.assertEqual(set(metadata["hostPackages"]["claude"]["files"]), {*build.CLAUDE_PACKAGE_FILES, "bin/delm"})
            (source / "hosts/claude/hooks/protocol.js").unlink()
            with self.assertRaises((OSError, RuntimeError)):
                package_release.assemble(source, runtime, root / "missing", "example/delm", revision)
            self.assertFalse((root / "missing/release.json").exists())

    def test_public_qualification_requires_actual_claude_evidence_for_each_architecture(self):
        with fixture() as (root, source, runtime, revision):
            codex = qualifications(root, source, runtime, revision)
            output = root / "unsigned"
            package_release.assemble(source, runtime, output, "example/delm", revision, qualifications=codex)
            self.assertEqual(package_release.verify(output)["claudeQualifiedArchitectures"], [])
            with self.assertRaisesRegex(RuntimeError, "arm64, x86_64"):
                package_release.verify(output, require_qualified=True)
            native = fixture_claude_qualifications(root, source, runtime, ["arm64"])
            arm = root / "arm"
            package_release.assemble(source, runtime, arm, "example/delm", revision,
                                     qualifications=codex, claude_qualifications=native)
            self.assertEqual(package_release.verify(arm)["claudeQualifiedArchitectures"], ["arm64"])
            with self.assertRaisesRegex(RuntimeError, "for x86_64"):
                package_release.verify(arm, require_qualified=True)
            native = fixture_claude_qualifications(root, source, runtime)
            array = root / "records.json"
            package_release.write_json(array, [dict(json.loads(path.read_text()), account="never distribute this") for path in native])
            both = root / "both"
            package_release.assemble(source, runtime, both, "example/delm", revision,
                                     qualifications=codex, claude_qualifications=[array])
            self.assertEqual(package_release.verify(both, require_qualified=True)["claudeQualifiedArchitectures"], ["arm64", "x86_64"])
            self.assertNotIn("never distribute this", (both / "release.json").read_text())

    def test_claude_release_proof_rejects_stale_bytes_sources_fixture_and_false_claims(self):
        for field, value in [("runtimeSha256", "0" * 64), ("runtimeSourcesSha256", "0" * 64),
                             ("fixtureSha256", "0" * 64), ("adapterSha256", "0" * 64), ("hostVersion", "2.1.288 (Claude Code)"),
                             ("bothWorkersPublishedFiles", False), ("matchingNativeToolPools", False),
                             ("workspacesRemoved", False), ("deliveredOutputChecked", False),
                             ("workerCount", 1), ("modelCalls", 0), ("passed", False), ("outputProof", {"../secret": "a" * 64})]:
            with self.subTest(field=field), fixture() as (root, source, runtime, revision):
                paths = fixture_claude_qualifications(root, source, runtime)
                record = json.loads(paths[0].read_text())
                record[field] = value
                package_release.write_json(paths[0], record)
                with self.assertRaisesRegex(RuntimeError, "native Claude qualification"):
                    package_release.assemble(source, runtime, root / "invalid", "example/delm", revision,
                                             claude_qualifications=paths)
                self.assertFalse((root / "invalid/release.json").exists())
        with fixture() as (root, source, runtime, revision):
            paths = fixture_claude_qualifications(root, source, runtime)
            with self.assertRaisesRegex(RuntimeError, "Duplicate Claude"):
                package_release.assemble(source, runtime, root / "duplicate", "example/delm", revision,
                                         claude_qualifications=paths + paths)

    def test_claude_version_and_source_symlinks_are_rejected(self):
        with fixture() as (root, source, runtime, revision):
            manifest = source / "hosts/claude/.claude-plugin/plugin.json"
            value = json.loads(manifest.read_text())
            value["version"] = "99.0.0"
            package_release.write_json(manifest, value)
            with self.assertRaisesRegex(RuntimeError, "must identify"):
                package_release.source_version(source)
        with fixture() as (root, source, runtime, revision):
            module = source / "hosts/claude/hooks/protocol.js"
            module.unlink()
            module.symlink_to(source / "hosts/claude/hooks/delm.js")
            with self.assertRaises((OSError, RuntimeError)):
                build.stage_package(source, runtime, root / "linked", "claude")

    def test_claude_catalog_runtime_and_validation_cannot_be_resealed_incorrectly(self):
        for mutation in ["catalog", "runtime", "extra", "validation", "host-path"]:
            with self.subTest(mutation=mutation), fixture() as (root, source, runtime, revision):
                output = root / "review"
                package_release.assemble(source, runtime, output, "example/delm", revision)
                metadata = json.loads((output / "release.json").read_text())
                package = output / "plugins/delm-claude"
                if mutation == "catalog":
                    path = output / ".claude-plugin/marketplace.json"
                    catalog = json.loads(path.read_text())
                    catalog["plugins"][0]["source"] = "../elsewhere"
                    package_release.write_json(path, catalog)
                elif mutation == "runtime":
                    (package / "bin/delm").write_text("different runtime")
                    metadata["hostPackages"]["claude"]["files"] = package_files(package)
                elif mutation == "extra":
                    (package / "private.txt").write_text("never ship")
                    metadata["hostPackages"]["claude"]["files"] = package_files(package)
                elif mutation == "validation":
                    metadata["claudeValidation"]["payloadSha256"] = "0" * 64
                else:
                    metadata["hostPackages"]["claude"]["path"] = "plugins/delm"
                package_release.write_json(output / "release.json", metadata)
                checksums(output)
                with self.assertRaisesRegex(RuntimeError, "catalog|identical shared|allowlist|validation|manifest"):
                    package_release.verify(output)

    def test_claude_untracked_input_changes_provenance(self):
        with fixture() as (_, source, _, _):
            before = package_release.source_state(source)
            (source / "hosts/claude/hooks/additional.js").write_text("// new adapter source")
            after = package_release.source_state(source)
            self.assertTrue(after["sourceDirty"])
            self.assertNotEqual(before["runtimeSourcesSha256"], after["runtimeSourcesSha256"])

    def test_native_claude_validator_uses_isolated_config_and_detects_mutation(self):
        with fixture() as (root, source, runtime, _):
            package = root / "plugin"
            build.stage_package(source, runtime, package, "claude")
            calls = []

            def run(command, **kwargs):
                calls.append((command, kwargs))
                if command[-1] == "--version":
                    return subprocess.CompletedProcess(command, 0, "2.1.289 (Claude Code)\n", "")
                return subprocess.CompletedProcess(command, 0, json.dumps({"success": True, "strict": True}), "")

            with mock.patch.object(build.shutil, "which", return_value="/fixture/claude"), \
                 mock.patch.object(build.subprocess, "run", side_effect=run):
                evidence = build.validate_claude_package(package)
            self.assertTrue(evidence["passed"])
            self.assertEqual(evidence["modelCalls"], 0)
            self.assertEqual(calls[1][0][1:], ["plugin", "validate", str(package.resolve()), "--strict", "--json"])
            environment = calls[1][1]["env"]
            self.assertNotEqual(environment["HOME"], str(Path.home()))
            self.assertEqual(Path(environment["CLAUDE_CONFIG_DIR"]).parent, Path(environment["HOME"]))
            self.assertFalse(Path(environment["HOME"]).exists())

            def mutate(command, **kwargs):
                if "validate" in command:
                    (package / "hooks/delm.js").write_text("changed during validation")
                return run(command, **kwargs)

            with mock.patch.object(build.shutil, "which", return_value="/fixture/claude"), \
                 mock.patch.object(build.subprocess, "run", side_effect=mutate), \
                 self.assertRaisesRegex(RuntimeError, "changed during"):
                build.validate_claude_package(package)

    def test_inheritance_proof_is_bound_to_source_runtime_architecture_and_host(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary)
            for name in qualify_release.INHERITANCE_INPUTS:
                path = source / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(name + " fixture bytes\n")
            proof = {
                "kind": "native-inheritance", "model_turns": 0,
                "saved_skill_contents_match": True, "saved_mcp_tools_match": True,
                "native_permission_profile_match": True, "mcp_tool_called": True,
                "delm_gateway_tool_called": True, "parent_cli_overrides_not_exported": True,
                "exact_live_session_parity": False, "runtime_sha256": "a" * 64,
                "host_version": "codex fixture", "architecture": "aarch64",
                "native_test_sha256": fingerprint(source / "tests/native_inheritance.rs")["sha256"],
                "source_digest": qualify_release.inheritance_source_digest(source),
                "skills": [{"path": "/private/fixture/path"}],
            }
            checked = qualify_release.validate_inheritance(proof, source, "a" * 64, "arm64", "codex fixture")
            self.assertNotIn("private", json.dumps(checked))
            for key, value in [("runtime_sha256", "b" * 64), ("architecture", "x86_64"),
                               ("host_version", "different"), ("native_test_sha256", "c" * 64),
                               ("source_digest", "d" * 64), ("exact_live_session_parity", True),
                               ("delm_gateway_tool_called", False)]:
                with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "inheritance evidence"):
                    qualify_release.validate_inheritance(dict(proof, **{key: value}), source,
                                                         "a" * 64, "arm64", "codex fixture")
            (source / "src/worker_tools.rs").write_text("changed adapter")
            with self.assertRaisesRegex(RuntimeError, "inheritance evidence"):
                qualify_release.validate_inheritance(proof, source, "a" * 64, "arm64", "codex fixture")

    def test_package_qualification_requires_native_inheritance_evidence(self):
        with fixture() as (root, source, runtime, revision):
            records = qualifications(root, source, runtime, revision)
            metadata = {"sourceRevision": revision, **package_release.source_state(source),
                        "qualification": {json.loads(path.read_text())["architecture"]:
                                          json.loads(path.read_text()) for path in records}}
            package_release.verify_qualification(metadata)
            for key in ["nativeInheritance", "evidenceSha256", "exactLiveSessionParity", "codexVersion"]:
                incomplete = json.loads(json.dumps(metadata))
                incomplete["qualification"]["arm64"].pop(key)
                with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "native release qualification"):
                    package_release.verify_qualification(incomplete)

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
                metadata["hostPackages"]["codex"]["files"] = metadata["files"]
                package_release.write_json(output / "release.json", metadata)
                checksums(output)
                with self.assertRaisesRegex(RuntimeError, "allowlist|unexpected artifacts"):
                    package_release.verify(output)

    def test_publisher_enforces_signed_evidence_before_any_remote_operation(self):
        with fixture() as (root, source, runtime, revision):
            records = qualifications(root, source, runtime, revision)
            origin, signed = root / "unsigned", root / "signed"
            package_release.assemble(source, runtime, origin, "example/delm", revision,
                                     qualifications=records,
                                     claude_qualifications=fixture_claude_qualifications(root, source, runtime))
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
        for host, relative in package_release.HOST_PATHS.items():
            with self.subTest(host=host), fixture() as (root, source, runtime, revision):
                records = qualifications(root, source, runtime, revision)
                origin = root / "unsigned"
                package_release.assemble(source, runtime, origin, "example/delm", revision,
                                         qualifications=records,
                                     claude_qualifications=fixture_claude_qualifications(root, source, runtime))
                package = origin / relative
                skill = package / "skills/run/SKILL.md"
                skill.write_text(skill.read_text() + "\nAltered payload\n")
                metadata = json.loads((origin / "release.json").read_text())
                metadata["hostPackages"][host]["files"] = package_files(package)
                if host == "codex":
                    metadata["files"] = metadata["hostPackages"][host]["files"]
                else:
                    metadata["claudeValidation"] = fixture_claude_validation(package)
                    metadata["claudeAdapterSha256"] = build.claude_adapter_digest(package)
                    for record in metadata["claudeQualification"].values():
                        record["adapterSha256"] = metadata["claudeAdapterSha256"]
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
                                     qualifications=records,
                                     claude_qualifications=fixture_claude_qualifications(root, source, runtime))
            package_release.verify(root / "review", require_qualified=True)
            record = json.loads(records[1].read_text())
            record["runtimeSha256"] = "0" * 64
            record["nativeInheritance"]["runtimeSha256"] = "0" * 64
            package_release.write_json(records[1], record)
            with self.assertRaisesRegex(RuntimeError, "differs from the tested binary"):
                package_release.assemble(source, runtime, root / "mismatch", "example/delm", revision,
                                         qualifications=records,
                                     claude_qualifications=fixture_claude_qualifications(root, source, runtime))

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
                                     qualifications=records,
                                     claude_qualifications=fixture_claude_qualifications(root, source, runtime))
            package_release.assemble(source, runtime, signed, "example/delm", revision,
                                     signed=True, unsigned_origin=origin)
            reports = root / "reports"
            for architecture in package_release.ARCHITECTURES:
                package_release.write_json(reports / ("signed-qualification-" + architecture) / "result.json", {
                    "kind": "release-runtime-smoke", "architecture": architecture,
                    "runtimeSha256": fingerprint(runtime)["sha256"], "passed": True,
                    "signedAndNotarized": True, "modelCalls": 0, **package_release.source_state(source),
                    "cases": [{"case": case, "passed": True, "workerCount": 2,
                               "originalPreserved": True, "resultDelivered": case == "complete",
                               "workspacesRemoved": True} for case in ["complete", "stop"]]})
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
