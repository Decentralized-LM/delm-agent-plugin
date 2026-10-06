"""Runtime packaging and real stock-plugin lifecycle tests in temporary CODEX_HOME."""

import contextlib
import io
import json
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

import build
import install_support
import package_release
import publish_release


SOURCE = Path(__file__).resolve().parent.parent
CODEX = shutil.which("codex")
CLAUDE = build.claude_executable()


def executable(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)
    path.chmod(0o755)


def fixture_runtime(path, version="fixture", board=True):
    replies = [
        {"jsonrpc": "2.0", "id": 1, "result": {"serverInfo": {"name": "delm"}}},
        {"jsonrpc": "2.0", "id": 2, "result": {"tools": [{"name": name} for name in
            ["delm_status", "delm_complete", "delm_service"]]}},
    ]
    script = '#!/bin/sh\n'
    if board:
        script += ('if [ "$1" = claude ] && [ "$2" = view ]; then\n'
                   "echo '--run-id --session-id --watch --interval-ms --collection --through-sequence --item-id'\nexit 0\nfi\n")
    script += 'if [ "$1" = claude ]; then\n'
    script += "printf '%s\\n' '" + "' '".join(json.dumps(reply) for reply in replies) + "'\nexit 0\nfi\n"
    script += "echo 'delm " + version + "'\n"
    executable(path, script)


def fixture_source(path):
    for directory in (".codex-plugin", ".agents/plugins", "skills/run/agents", "hooks", "scripts"):
        (path / directory).mkdir(parents=True, exist_ok=True)
    for name in (".codex-plugin/plugin.json", ".agents/plugins/marketplace.json",
                 "skills/run/agents/openai.yaml", "hooks/hooks.json", "Cargo.toml", "Cargo.lock",
                 "rust-toolchain.toml", "LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.txt"):
        shutil.copy2(SOURCE / name, path / name)
    for name in ("src", "plugin"):
        shutil.copytree(SOURCE / name, path / name)
    (path / "skills/run/SKILL.md").write_text("---\nname: run\ndescription: Run an explicitly requested DeLM task.\n---\nUse the bundled runtime.\n")
    for name in ("install_support.py", "install.sh", "uninstall.sh", "migrate.sh", "build.py", "build.sh", "dependency_notices.py", "verify_claude_native.py"):
        shutil.copy2(SOURCE / "scripts" / name, path / "scripts" / name)
    version = json.loads((path / ".codex-plugin/plugin.json").read_text())["version"]
    package_release.write_json(path / "hosts/claude/.claude-plugin/plugin.json", {
        "name": "delm", "version": version, "description": "Native package fixture",
        "author": {"name": "Fixture"}, "license": "MIT",
    })
    package_release.write_json(path / "hosts/claude/.mcp.json", {"mcpServers": {"delm": {
        "command": "${CLAUDE_PLUGIN_ROOT}/bin/delm", "args": ["claude", "mcp"],
    }}})
    package_release.write_json(path / "hosts/claude/hooks/hooks.json", {"modules": ["./hooks/delm.js"]})
    (path / "hosts/claude/hooks/delm.js").write_text("// Unit-test package fixture; not a working host adapter.\n")
    for name in ("protocol.js", "board-view.js", "board-render.js"):
        (path / "hosts/claude/hooks" / name).write_text("export const fixture = true;\n")
    skill = path / "hosts/claude/skills/run/SKILL.md"
    skill.parent.mkdir(parents=True)
    skill.write_text("---\nname: run\ndescription: Unit-test package fixture.\n---\nDo not run.\n")
    return path


def fixture_claude_validation(package, claude="claude"):
    return {"kind": "claude-plugin-validation", "hostVersion": "fixture Claude", "strict": True,
            "passed": True, "modelCalls": 0, "payloadSha256": build.payload_digest(package)}



def fixture_claude_qualifications(root, source, runtime, architectures=package_release.ARCHITECTURES):
    paths = []
    with tempfile.TemporaryDirectory(dir=root) as temporary:
        package = Path(temporary) / "plugin"
        build.stage_package(source, runtime, package, "claude")
        adapter_sha = build.claude_adapter_digest(package)
    for arch in architectures:
        path = root / ("claude-" + arch + ".json")
        package_release.write_json(path, {
            "schema": 1, "kind": "claude-native-qualification", "architecture": arch,
            "hostVersion": "2.1.289 (Claude Code)", "passed": True, "workerCount": 2, "modelCalls": 3,
            "runtimeSha256": install_support.fingerprint(runtime)["sha256"],
            "runtimeSourcesSha256": package_release.source_state(source)["runtimeSourcesSha256"],
            "fixtureSha256": install_support.fingerprint(source / "scripts/verify_claude_native.py")["sha256"],
            "adapterSha256": adapter_sha,
            **{flag: True for flag in package_release.CLAUDE_PROOF_FLAGS},
            "outputProof": {name: "a" * 64 for name in package_release.CLAUDE_PROOF_FILES},
        })
        paths.append(path)
    return paths


def fixture_package(source):
    package = source / ".build/plugin"
    package.mkdir(parents=True)
    for name in (".codex-plugin", "skills", "hooks"):
        shutil.copytree(source / name, package / name)
    for name in ("LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.txt"):
        shutil.copy2(source / name, package / name)
    executable(package / "bin/delm", "#!/bin/sh\necho 'delm fixture'\n")
    return package


class LocalClaudeCatalogTests(unittest.TestCase):
    def test_source_catalog_selects_only_the_staged_claude_payload(self):
        catalog = json.loads((SOURCE / ".claude-plugin/marketplace.json").read_text())
        self.assertEqual(catalog["name"], "delm-local")
        self.assertTrue(catalog["owner"]["name"])
        self.assertEqual(catalog["plugins"], [{"name": "delm", "source": "./.build/plugin-claude"}])
        codex = json.loads((SOURCE / ".agents/plugins/marketplace.json").read_text())
        self.assertEqual(codex["plugins"][0]["source"]["path"], "./.build/plugin")


@unittest.skipUnless(CLAUDE and sys.platform == "darwin", "Native macOS Claude CLI is required")
class LocalClaudeInstallationTests(unittest.TestCase):
    def test_native_directory_install_rebuild_refresh_and_removal_preserve_settings(self):
        with tempfile.TemporaryDirectory(prefix="delm Claude source catalog ") as temporary:
            root = Path(temporary).resolve()
            home, config, source = root / "home", root / "config", root / "source with spaces"
            for directory in [home, config, source / ".claude-plugin"]:
                directory.mkdir(parents=True)
            shutil.copy2(SOURCE / ".claude-plugin/marketplace.json", source / ".claude-plugin/marketplace.json")
            (source / ".gitignore").write_text("/.build/\n")
            package = source / ".build/plugin-claude"
            manifest = package / ".claude-plugin/plugin.json"
            package_release.write_json(manifest, {"name": "delm", "version": "0.3.0",
                                                 "description": "Native directory-install fixture"})
            skill = package / "skills/run/SKILL.md"
            skill.parent.mkdir(parents=True)
            skill.write_text("---\nname: run\ndescription: Native installer fixture.\n---\nNever run a model task.\n")
            executable(package / "bin/delm", "#!/bin/sh\necho fixture-initial\n")
            settings = {"model": "fixture-model", "permissions": {"deny": ["Bash(unrelated-command)"]}}
            package_release.write_json(config / "settings.json", settings)
            (config / ".credentials.json").write_text("{}\n")
            env = {"HOME": str(home), "CLAUDE_CONFIG_DIR": str(config),
                   "PATH": os.environ.get("PATH", "/usr/bin:/bin"), "LANG": "en_US.UTF-8"}

            def native(*arguments):
                result = subprocess.run([CLAUDE, "plugin", *arguments, "--json"], cwd=root, env=env,
                                        text=True, capture_output=True, timeout=30)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                text = result.stdout.strip()
                return json.loads(text if text.startswith("[") else text.splitlines()[-1])

            native("marketplace", "add", str(source), "--scope", "user")
            native("install", "delm@delm-local", "--scope", "user")
            installed = next(entry for entry in native("list") if entry["id"] == "delm@delm-local")
            self.assertEqual(installed["scope"], "user")
            self.assertTrue(installed["enabled"])
            self.assertEqual(Path(installed["readFromFolder"]), package)
            # Directory sources resolve rebuilt files directly, including the
            # ignored build folder, without requiring a manifest version bump.
            executable(package / "bin/delm", "#!/bin/sh\necho fixture-rebuilt\n")
            rebuilt = next(entry for entry in native("list") if entry["id"] == "delm@delm-local")
            self.assertEqual(Path(rebuilt["readFromFolder"]), package)
            self.assertIn("fixture-rebuilt", (Path(rebuilt["readFromFolder"]) / "bin/delm").read_text())
            native("marketplace", "update", "delm-local")
            same_version = native("update", "delm@delm-local", "--scope", "user")
            self.assertEqual(same_version["updateOutcome"], "up_to_date")
            package_release.write_json(manifest, {"name": "delm", "version": "0.3.1",
                                                 "description": "Native directory-install fixture"})
            native("marketplace", "update", "delm-local")
            native("update", "delm@delm-local", "--scope", "user")
            updated = next(entry for entry in native("list") if entry["id"] == "delm@delm-local")
            self.assertEqual(updated["folderVersion"], "0.3.1")
            removed = native("uninstall", "delm@delm-local", "--scope", "user", "--keep-data")
            self.assertTrue(removed["keptData"])
            self.assertFalse(any(entry["id"] == "delm@delm-local" for entry in native("list")))
            self.assertTrue(any(entry["name"] == "delm-local" for entry in native("marketplace", "list")))
            self.assertTrue((package / "bin/delm").exists())
            native("marketplace", "remove", "delm-local")
            self.assertEqual(native("marketplace", "list"), [])
            final = json.loads((config / "settings.json").read_text())
            self.assertEqual(final["model"], settings["model"])
            self.assertEqual(final["permissions"], settings["permissions"])
            self.assertEqual((config / ".credentials.json").read_text(), "{}\n")


class InstallationPreflightTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="delm preflight ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.home = self.root / "codex home"
        self.home.mkdir()
        (self.home / "config.toml").write_text("# preserve config\n")
        self.source = self.root / "source"
        self.source.mkdir()
        self.tools = {name: "/tools/" + name for name in ("codex", "git", "cargo", "xcrun")}
        for patch in (
            mock.patch.object(install_support.sys, "platform", "darwin"),
            mock.patch.object(install_support, "SOURCE", self.source),
            mock.patch.object(install_support.shutil, "which", side_effect=self.tools.get),
        ):
            patch.start()
            self.addCleanup(patch.stop)
        patch = mock.patch.object(install_support.subprocess, "run")
        self.run = patch.start()
        self.addCleanup(patch.stop)
        self.run.return_value = subprocess.CompletedProcess([], 0, "/tools/clang\n", "")

    def invoke(self, operation="install", *flags):
        with mock.patch.object(sys, "argv", ["install_support.py", operation, "--codex", "codex", *flags]), \
             mock.patch.dict(os.environ, CODEX_HOME=str(self.home)):
            install_support.main()

    def assert_unchanged(self):
        self.assertEqual(list(self.source.iterdir()), [])
        self.assertEqual([path.name for path in self.home.iterdir()], ["config.toml"])
        self.assertEqual((self.home / "config.toml").read_text(), "# preserve config\n")

    def test_unsupported_platform_and_missing_codex_fail_before_mutation(self):
        with mock.patch.object(install_support.sys, "platform", "win32"):
            with self.assertRaisesRegex(RuntimeError, "macOS only"):
                self.invoke()
        del self.tools["codex"]
        with self.assertRaisesRegex(RuntimeError, "Install stock Codex CLI"):
            self.invoke()
        self.run.assert_not_called()
        self.assert_unchanged()

    def test_missing_home_explains_first_run_setup_without_creating_it(self):
        missing = self.root / "missing codex home"
        with self.assertRaisesRegex(RuntimeError, "Run Codex once"):
            install_support.preflight("install", "codex", missing)
        self.assertFalse(missing.exists())
        self.run.assert_not_called()
        self.assert_unchanged()

    def test_missing_native_commands_and_unresponsive_codex_fail_before_mutation(self):
        for outcome in (subprocess.CompletedProcess([], 2, "", "unknown subcommand"),
                        subprocess.TimeoutExpired("codex", 15)):
            with self.subTest(outcome=outcome):
                if isinstance(outcome, Exception):
                    self.run.side_effect = outcome
                else:
                    self.run.return_value = outcome
                with self.assertRaisesRegex(RuntimeError, "Codex.*plugin"):
                    self.invoke()
                self.assert_unchanged()
        self.assertTrue(all(call.args[0][-2:] == ["--json", "--help"] for call in self.run.call_args_list))

    def test_missing_source_tools_and_invalid_xcode_fail_before_mutation(self):
        for tool, message in (("git", "Git is required"), ("cargo", "Rust/Cargo"),
                              ("xcrun", "Xcode Command Line Tools")):
            with self.subTest(tool=tool), mock.patch.dict(self.tools):
                del self.tools[tool]
                with self.assertRaisesRegex(RuntimeError, message):
                    self.invoke()
                self.assert_unchanged()
        self.run.side_effect = lambda command, **kwargs: subprocess.CompletedProcess(
            command, 1 if command[0] == self.tools["xcrun"] else 0, "", "")
        with self.assertRaisesRegex(RuntimeError, "unavailable or not selected"):
            self.invoke()
        self.assert_unchanged()

    def test_staged_install_and_removal_do_not_require_build_tools(self):
        del self.tools["cargo"], self.tools["xcrun"]
        for operation, build in (("install", False), ("uninstall", True), ("migrate", True)):
            with self.subTest(operation=operation):
                self.assertEqual(install_support.preflight(operation, "codex", self.home, build), "/tools/codex")
        self.assertTrue(all(call.args[0][-1] == "--help" for call in self.run.call_args_list))
        self.assert_unchanged()


@unittest.skipUnless(CODEX, "Existing stock Codex is required for isolated native plugin tests")
class NativeInstallationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="delm native plugin fixtures ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.source = fixture_source(self.root / "source with spaces")
        self.package = fixture_package(self.source)
        self.codex_home = self.root / "isolated codex home"
        self.codex_home.mkdir()
        self.cwd = self.root / "unrelated working directory"
        self.cwd.mkdir()
        self.installation = install_support.Installation(self.source, self.codex_home, CODEX, self.cwd)
        # These are fixtures inside the temporary home, never the user's account files.
        (self.codex_home / "auth.json").write_text('{}\n')
        (self.codex_home / "config.toml").write_text('model = "fixture-model"\n')
        (self.root / "previous patched host").mkdir()
        (self.root / "previous patched host/qualification.txt").write_text("preserve evidence")
        (self.root / "results.txt").write_text("preserve results")

    def invoke(self, operation, success=True):
        result = subprocess.run([str(self.source / "scripts" / (operation + ".sh")),
                                 "--no-build", "--codex", CODEX], cwd=self.cwd,
                                env=dict(os.environ, CODEX_HOME=str(self.codex_home), PYTHONDONTWRITEBYTECODE="1"),
                                text=True, capture_output=True)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return result

    def test_native_install_upgrade_uninstall_and_repeat_keep_unrelated_data(self):
        self.invoke("install")
        first = self.installation.receipt()
        cached = Path(first["installed_path"])
        self.assertTrue((cached / "bin/delm").is_file())
        self.assertFalse((self.codex_home / "bin/codex-delm").exists())
        executable(self.package / "bin/delm", "#!/bin/sh\necho updated-fixture\n")
        self.invoke("install")
        self.assertIn("updated-fixture", (cached / "bin/delm").read_text())
        self.invoke("uninstall")
        self.invoke("uninstall")
        self.assertFalse(cached.exists())
        self.assertNotIn("delm", (self.codex_home / "config.toml").read_text())
        self.assertIn('model = "fixture-model"', (self.codex_home / "config.toml").read_text())
        self.assertEqual((self.codex_home / "auth.json").read_text(), '{}\n')
        self.assertEqual((self.root / "previous patched host/qualification.txt").read_text(), "preserve evidence")
        self.assertEqual((self.root / "results.txt").read_text(), "preserve results")
        self.assertTrue((self.package / "bin/delm").exists())

    def test_modified_cached_files_are_preserved(self):
        self.invoke("install")
        cached = Path(self.installation.receipt()["installed_path"])
        (cached / "user-note.txt").write_text("keep me")
        self.assertIn("files changed", self.invoke("uninstall", success=False).stderr)
        self.assertIn("files changed", self.invoke("install", success=False).stderr)
        self.assertEqual((cached / "user-note.txt").read_text(), "keep me")
        self.assertTrue(self.installation.installed(self.installation.marketplace()))

    def test_install_refuses_staged_portable_manifest_that_disables_native_hooks(self):
        (self.package / "plugin.json").write_text('{"name":"delm","version":"0.3.0"}')
        self.assertIn("disables native hooks", self.invoke("install", success=False).stderr)
        self.assertIsNone(self.installation.receipt())
        self.assertIsNone(self.installation.marketplace())

    def test_migration_requires_public_plugin_and_preserves_modified_local_cache(self):
        self.invoke("install")
        cached = Path(self.installation.receipt()["installed_path"])
        (cached / "user-note.txt").write_text("preserve my changes")
        self.invoke("migrate", success=False)
        self.assertTrue(cached.exists())
        public = self.root / "public catalog"
        package_release.write_json(public / ".agents/plugins/marketplace.json", {
            "name": "delm", "plugins": [{"name": "delm", "source": {
                "source": "local", "path": "./plugin"}}]})
        shutil.copytree(self.package, public / "plugin")
        self.installation.native("marketplace", "add", str(public))
        self.installation.native("add", "delm@delm")
        self.invoke("migrate")
        self.invoke("migrate")
        receipt = self.installation.receipt()
        archive = Path(receipt["preserved_path"])
        self.assertEqual((archive / cached.name / "user-note.txt").read_text(), "preserve my changes")
        self.assertFalse(cached.exists())
        entries = self.installation.native("list", "--marketplace", "delm")["installed"]
        self.assertEqual([entry["pluginId"] for entry in entries], ["delm@delm"])
        self.assertEqual((self.root / "results.txt").read_text(), "preserve results")

    def test_stock_app_server_discovers_skill_and_untrusted_hooks_without_a_model_turn(self):
        output = self.invoke("install").stdout
        self.assertIn("/hooks", output)
        self.assertIn("does not grant hook trust", output)
        process = subprocess.Popen([CODEX, "app-server"], cwd=self.cwd,
                                   env=dict(os.environ, CODEX_HOME=str(self.codex_home)),
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=subprocess.DEVNULL, start_new_session=True)

        def send(message):
            process.stdin.write((json.dumps(message) + "\n").encode())
            process.stdin.flush()

        def response(request_id):
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                if not select.select([process.stdout], [], [], max(0, deadline - time.monotonic()))[0]:
                    break
                line = process.stdout.readline()
                if not line:
                    break
                message = json.loads(line)
                if message.get("id") == request_id:
                    self.assertNotIn("error", message)
                    return message["result"]
            self.fail(f"No stock app-server response for request {request_id}")

        try:
            send({"id": 1, "method": "initialize", "params": {
                "clientInfo": {"name": "delm_plugin_test", "version": package_release.source_version(self.source)},
                "capabilities": {"experimentalApi": True}}})
            response(1)
            send({"method": "initialized"})
            send({"id": 2, "method": "skills/list", "params": {
                "cwds": [str(self.cwd)], "forceReload": True}})
            entries = response(2)["data"]
            skills = [skill for entry in entries for skill in entry["skills"] if skill["name"] == "delm:run"]
            self.assertEqual(len(skills), 1)
            self.assertTrue(skills[0]["enabled"])
            self.assertEqual(skills[0]["pluginId"], install_support.PLUGIN_ID)
            self.assertEqual(skills[0]["interface"]["displayName"], "DeLM")
            self.assertEqual(skills[0]["interface"]["shortDescription"], "Build with two collaborating agents.")
            self.assertEqual(Path(skills[0]["path"]), Path(self.installation.receipt()["installed_path"]) / "skills/run/SKILL.md")
            send({"id": 3, "method": "hooks/list", "params": {"cwds": [str(self.cwd)]}})
            entries = response(3)["data"]
            self.assertTrue(all(not entry["errors"] for entry in entries), entries)
            hooks = [hook for entry in entries for hook in entry["hooks"]
                     if hook["pluginId"] == install_support.PLUGIN_ID]
            expected = json.loads((self.package / "hooks/hooks.json").read_text())["hooks"]
            expected_names = {name[0].lower() + name[1:] for name in expected}
            self.assertEqual({hook["eventName"] for hook in hooks}, expected_names, entries)
            self.assertEqual(len(hooks), sum(len(group["hooks"]) for groups in expected.values() for group in groups))
            for hook in hooks:
                self.assertEqual(hook["source"], "plugin")
                self.assertEqual(hook["trustStatus"], "untrusted")
                self.assertTrue(hook["enabled"])
                self.assertTrue(hook["currentHash"])
                self.assertEqual(Path(hook["sourcePath"]), Path(self.installation.receipt()["installed_path"]) / "hooks/hooks.json")
                self.assertIn("lifecycle-hook", hook["command"])
            self.assertNotIn("trusted_hash", (self.codex_home / "config.toml").read_text())
            # Model an explicit /hooks approval in this disposable home using
            # the same native config write as Codex's hook-review UI.
            send({"id": 4, "method": "config/batchWrite", "params": {
                "edits": [{"keyPath": "hooks.state", "mergeStrategy": "upsert", "value": {
                    hook["key"]: {"trusted_hash": hook["currentHash"]} for hook in hooks}}],
                "reloadUserConfig": True}})
            response(4)
            send({"id": 5, "method": "hooks/list", "params": {"cwds": [str(self.cwd)]}})
            trusted = [hook for entry in response(5)["data"] for hook in entry["hooks"]
                       if hook["pluginId"] == install_support.PLUGIN_ID]
            self.assertEqual(len(trusted), len(hooks))
            self.assertTrue(all(hook["trustStatus"] == "trusted" for hook in trusted), trusted)
        finally:
            process.stdin.close()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGTERM)
                process.wait(timeout=5)
            process.stdout.close()

    def test_foreign_marketplace_is_not_replaced_or_removed(self):
        foreign = fixture_source(self.root / "other source")
        fixture_package(foreign)
        self.installation.native("marketplace", "add", str(foreign))
        before = (self.codex_home / "config.toml").read_bytes()
        self.assertIn("another source", self.invoke("install", success=False).stderr)
        self.invoke("uninstall")
        self.assertEqual((self.codex_home / "config.toml").read_bytes(), before)

    def test_uninstall_retains_preexisting_same_source_marketplace(self):
        self.installation.native("marketplace", "add", str(self.source))
        self.invoke("install")
        self.assertFalse(self.installation.receipt()["owns_marketplace"])
        self.invoke("uninstall")
        self.assertIsNotNone(self.installation.marketplace())
        self.assertFalse(self.installation.installed(self.installation.marketplace()))

    def test_install_refuses_preexisting_plugin_without_our_receipt(self):
        self.installation.native("marketplace", "add", str(self.source))
        outcome = self.installation.native("add", install_support.PLUGIN_ID)
        self.assertIn("outside this installer", self.invoke("install", success=False).stderr)
        self.invoke("uninstall")
        self.assertTrue(Path(outcome["installedPath"]).exists())

    def test_failed_native_plugin_add_can_be_cleaned_up(self):
        (self.package / ".codex-plugin/plugin.json").write_text('{"name":"mismatched-plugin"}')
        self.invoke("install", success=False)
        self.assertIsNotNone(self.installation.receipt())
        self.invoke("uninstall")
        self.assertIsNone(self.installation.marketplace())

    def test_failed_upgrade_retains_previous_cache_receipt(self):
        self.invoke("install")
        before = self.installation.receipt()
        (self.package / ".codex-plugin/plugin.json").write_text('{"name":"mismatched-plugin"}')
        self.invoke("install", success=False)
        self.assertEqual(self.installation.receipt()["files"], before["files"])
        self.assertEqual(self.installation.receipt()["installed_path"], before["installed_path"])
        self.invoke("uninstall")

    def test_reinstall_does_not_claim_a_new_preexisting_marketplace(self):
        self.invoke("install")
        self.invoke("uninstall")
        self.installation.native("marketplace", "add", str(self.source))
        self.invoke("install")
        self.assertFalse(self.installation.receipt()["owns_marketplace"])
        self.invoke("uninstall")
        self.assertIsNotNone(self.installation.marketplace())


class BuildTests(unittest.TestCase):
    def setUp(self):
        validator = mock.patch.object(package_release, "validate_claude_package", side_effect=fixture_claude_validation)
        self.native_validation = validator.start()
        self.addCleanup(validator.stop)

    def test_release_package_has_pinned_catalog_integrity_and_no_source_artifacts(self):
        with tempfile.TemporaryDirectory(prefix="delm release fixture ") as temporary:
            source = fixture_source(Path(temporary) / "source")
            runtime = Path(temporary) / "delm"
            version = package_release.source_version(source)
            fixture_runtime(runtime, version)
            (source / "skills/run/local-notes.txt").write_text("never ship local notes")
            output = Path(temporary) / "release"
            real_run = subprocess.run

            def run(command, **kwargs):
                if command[0] == "lipo":
                    return subprocess.CompletedProcess(command, 0)
                return real_run(command, **kwargs)

            with mock.patch.object(package_release.subprocess, "run", side_effect=run):
                package_release.assemble(source, runtime, output, "example/delm", "a" * 40)
            package_release.verify(output)
            catalog = json.loads((output / ".agents/plugins/marketplace.json").read_text())
            self.assertEqual(catalog["name"], "delm")
            self.assertEqual(catalog["plugins"][0]["source"]["ref"], f"delm-plugin-v{version}")
            self.assertEqual(set(path.name for path in (output / "plugins/delm").iterdir()),
                             {"bin", "skills", "hooks", ".codex-plugin", "LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.txt"})
            self.assertEqual((output / "plugins/delm/bin/delm").stat().st_mode & 0o777, 0o755)
            self.assertFalse((output / "plugins/delm/skills/run/local-notes.txt").exists())
            metadata = json.loads((output / "release.json").read_text())
            self.assertIn("hooks/hooks.json", metadata["files"])
            self.assertEqual(set(metadata["hostPackages"]), {"codex", "claude"})
            self.assertEqual(metadata["hostPackages"]["claude"]["files"]["bin/delm"], metadata["files"]["bin/delm"])
            self.assertEqual((output / "plugins/delm-claude/hooks/worker.md").read_bytes(),
                             (source / "plugin/worker.md").read_bytes())
            self.assertEqual(json.loads((output / ".claude-plugin/marketplace.json").read_text())["plugins"][0]["source"],
                             "./plugins/delm-claude")
            self.assertIn("plugins/delm/hooks/hooks.json", (output / "SHA256SUMS").read_text())
            (output / "plugins/delm/extra.txt").write_text("unexpected")
            with self.assertRaisesRegex(RuntimeError, "changed"):
                package_release.verify(output)

    def test_release_refuses_mismatched_versions_and_existing_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = fixture_source(Path(temporary) / "source")
            (source / ".codex-plugin/plugin.json").write_text('{"name":"delm","version":"99.0.0"}')
            with self.assertRaisesRegex(RuntimeError, "must identify"):
                package_release.source_version(source)
            with self.assertRaisesRegex(RuntimeError, "preserving"):
                package_release.assemble(source, Path("missing"), source, "example/delm", "a" * 40)

    def test_only_runtime_is_built_and_previous_package_is_preserved(self):
        with tempfile.TemporaryDirectory(prefix="delm runtime build fixture ") as temporary:
            source = fixture_source(Path(temporary) / "source")
            old = fixture_package(source)
            (old / "qualification.txt").write_text("keep previous evidence")
            commands = []

            def run(command, **kwargs):
                commands.append(command)
                if command[0] == "cargo":
                    target = Path(command[command.index("--target-dir") + 1])
                    executable(target / "release/delm", "#!/bin/sh\necho delm-runtime-fixture\n")
                return subprocess.CompletedProcess(command, 0)

            with mock.patch.object(build.subprocess, "run", side_effect=run), mock.patch.object(build.shutil, "which", return_value="cargo"), contextlib.redirect_stdout(io.StringIO()):
                build.build(source)
            self.assertEqual(len(commands), 2)
            self.assertEqual(commands[0][:7], ["cargo", "build", "--locked", "--release", "--bin", "delm", "--manifest-path"])
            self.assertEqual(set(path.name for path in (source / ".build/plugin/bin").iterdir()), {"delm"})
            self.assertEqual(len(list((source / ".build").glob("plugin-previous.*/qualification.txt"))), 1)
            self.assertFalse((source / ".build/plugin/host").exists())
            self.assertFalse((source / ".build/plugin/qualification.txt").exists())

    def test_both_hosts_share_one_build_and_only_native_payloads(self):
        with tempfile.TemporaryDirectory(prefix="delm both-host build ") as temporary:
            source = fixture_source(Path(temporary) / "source")
            (source / "hosts/claude/private-notes.txt").write_text("never package this")
            calls = []
            real_run = subprocess.run

            def run(command, **kwargs):
                calls.append(command)
                if command[0] == "cargo":
                    target = Path(command[command.index("--target-dir") + 1])
                    fixture_runtime(target / "release/delm")
                    return subprocess.CompletedProcess(command, 0)
                return real_run(command, **kwargs)

            with mock.patch.object(build.subprocess, "run", side_effect=run), \
                 mock.patch.object(build.shutil, "which", return_value="cargo"), \
                 mock.patch.object(build, "validate_claude_package", side_effect=fixture_claude_validation) as validation, \
                 contextlib.redirect_stdout(io.StringIO()):
                build.build(source, host="all")
            self.assertEqual(sum(call[0] == "cargo" for call in calls), 1)
            validation.assert_called_once()
            self.assertEqual((source / ".build/plugin/bin/delm").read_bytes(),
                             (source / ".build/plugin-claude/bin/delm").read_bytes())
            self.assertEqual(set(install_support.package_files(source / ".build/plugin-claude")),
                             {*build.CLAUDE_PACKAGE_FILES, "bin/delm"})
            self.assertEqual((source / ".build/plugin-claude/hooks/worker.md").read_bytes(),
                             (source / "plugin/worker.md").read_bytes())

    def test_old_same_version_runtime_cannot_activate_claude_package(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = fixture_source(Path(temporary) / "source")
            original = fixture_package(source)
            before = install_support.package_files(original)
            runtime = Path(temporary) / "old-runtime"
            executable(runtime, "#!/bin/sh\necho 'delm fixture'\n")
            with mock.patch.object(build, "validate_claude_package") as validation, \
                 contextlib.redirect_stdout(io.StringIO()), \
                 self.assertRaisesRegex(RuntimeError, "native Claude MCP transport"):
                build.build(source, runtime, host="all")
            validation.assert_not_called()
            self.assertEqual(install_support.package_files(original), before)
            self.assertFalse((source / ".build/plugin-claude").exists())

    def test_runtime_without_board_observer_cannot_activate_new_claude_adapter(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = fixture_source(Path(temporary) / "source")
            original = fixture_package(source)
            before = install_support.package_files(original)
            runtime = Path(temporary) / "old-runtime"
            fixture_runtime(runtime, board=False)
            with mock.patch.object(build, "validate_claude_package") as validation, \
                 contextlib.redirect_stdout(io.StringIO()), \
                 self.assertRaisesRegex(RuntimeError, "Claude board observer"):
                build.build(source, runtime, host="all")
            validation.assert_not_called()
            self.assertEqual(install_support.package_files(original), before)
            self.assertFalse((source / ".build/plugin-claude").exists())

    def test_missing_or_invalid_claude_payload_cannot_replace_a_working_build(self):
        for failure in ["missing", "invalid"]:
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as temporary:
                source = fixture_source(Path(temporary) / "source")
                original = fixture_package(source)
                before = install_support.package_files(original)
                runtime = Path(temporary) / "runtime"
                fixture_runtime(runtime)
                if failure == "missing":
                    (source / "hosts/claude/hooks/delm.js").unlink()
                with mock.patch.object(build, "validate_claude_package", side_effect=RuntimeError("Invalid native payload")), \
                     contextlib.redirect_stdout(io.StringIO()), \
                     self.assertRaises((OSError, RuntimeError)):
                    build.build(source, runtime, host="all")
                self.assertEqual(install_support.package_files(original), before)
                self.assertFalse((source / ".build/plugin-claude").exists())

    def test_publication_uses_only_distribution_tree_and_never_replaces_a_tag(self):
        with tempfile.TemporaryDirectory(prefix="delm publishing fixture ") as temporary:
            root = Path(temporary).resolve()
            source = fixture_source(root / "source")
            remote = root / "remote.git"

            def git(*args, cwd=source):
                return subprocess.check_output(["git", *args], cwd=cwd, text=True,
                                               stderr=subprocess.DEVNULL).strip()

            git("init", "-b", "main")
            git("config", "user.name", "DeLM fixture")
            git("config", "user.email", "fixture@example.invalid")
            git("add", ".")
            git("commit", "-m", "Fixture source")
            revision = git("rev-parse", "HEAD")
            git("init", "--bare", str(remote))
            git("remote", "add", "origin", str(remote))
            runtime = root / "delm"
            version = package_release.source_version(source)
            fixture_runtime(runtime, version)
            output = root / "release"
            real_run = subprocess.run

            def run(command, **kwargs):
                if command[0] == "lipo":
                    if "-thin" in command:
                        shutil.copy2(command[1], command[-1])
                    return subprocess.CompletedProcess(command, 0)
                return real_run(command, **kwargs)

            with mock.patch.object(package_release.subprocess, "run", side_effect=run):
                qualifications = []
                provenance = package_release.source_state(source)
                for architecture, target in package_release.ARCHITECTURES.items():
                    path = root / (architecture + ".json")
                    package_release.write_json(path, {
                        "schema": 1, "kind": "native-release-build", "architecture": architecture,
                        "target": target, "sourceRevision": revision, **provenance,
                        "runtimeSha256": install_support.fingerprint(runtime)["sha256"],
                        "passed": True, "modelCalls": 0, "lifecycleCases": package_release.LIFECYCLE_CASES,
                        "codexVersion": "fixture 1", "exactLiveSessionParity": False,
                        "evidenceSha256": {name: "a" * 64 for name in ["smoke", "inheritance", *package_release.LIFECYCLE_CASES]},
                        "nativeInheritance": {
                            "runtimeSha256": install_support.fingerprint(runtime)["sha256"],
                            "architecture": architecture, "hostVersion": "fixture 1",
                            "gatewayToolCalled": True, "parentCliOverridesNotExported": True,
                            "exactLiveSessionParity": False, "nativeTestSha256": "b" * 64,
                            "sourceDigest": "c" * 64,
                        },
                    })
                    qualifications.append(path)
                unsigned = root / "unsigned"
                package_release.assemble(source, runtime, unsigned, "example/delm", revision,
                                         qualifications=qualifications,
                                         claude_qualifications=fixture_claude_qualifications(root, source, runtime))
                package_release.assemble(source, runtime, output, "example/delm", revision,
                                         signed=True, unsigned_origin=unsigned)
            previous_cwd = Path.cwd()
            try:
                os.chdir(source)
                with mock.patch.dict(os.environ, GITHUB_ACTIONS="true", RUNNER_TEMP=str(root),
                                     RELEASE_SOURCE_SHA=revision, RELEASE_REPOSITORY="example/delm"), \
                     mock.patch.object(publish_release, "verify_identity", return_value=revision), \
                     mock.patch.object(publish_release, "verify_destination"), \
                     mock.patch.object(publish_release, "verify_signed") as signed_gate, \
                     mock.patch.object(sys, "argv", ["publish_release.py", str(output), str(root / "reports")]):
                    publish_release.main()
                    signed_gate.assert_called_once_with(output, root / "reports")
                    with self.assertRaisesRegex(SystemExit, "already exists"):
                        publish_release.main()
            finally:
                os.chdir(previous_cwd)
            self.assertEqual(git("rev-parse", "HEAD"), revision)
            self.assertEqual(git("status", "--porcelain"), "")
            files = set(git("ls-tree", "-r", "--name-only", "marketplace", cwd=remote).splitlines())
            self.assertEqual(files, set(install_support.package_files(output)))
            self.assertEqual(git("rev-parse", "marketplace", cwd=remote),
                             git("rev-parse", f"refs/tags/delm-plugin-v{version}", cwd=remote))


@unittest.skipUnless(CODEX and shutil.which("git"), "Stock Codex and Git required")
class NativeReleaseTests(unittest.TestCase):
    def test_git_marketplace_version_upgrade_failure_and_removal(self):
        with tempfile.TemporaryDirectory(prefix="delm git release ") as temporary:
            root = Path(temporary).resolve()
            source = fixture_source(root / "source")
            repository = root / "release repository"
            repository.mkdir()
            package = repository / "plugins/delm"
            fixture = fixture_package(source)
            shutil.copytree(fixture, package)
            home = root / "codex home"
            home.mkdir()
            (home / "config.toml").write_text('model = "fixture-model"\n')
            (home / "auth.json").write_text('{}\n')
            retained = home / "delm/runs/example/result.txt"
            retained.parent.mkdir(parents=True)
            retained.write_text("saved result")
            git_config = root / "isolated git config"
            # Exercise Codex's real HTTPS Git path while redirecting transport to
            # a local fixture. No network, account, or real Git settings are used.
            url = "https://github.com/delm-test-fixture/release.git"
            subprocess.run(["git", "config", "--file", str(git_config),
                            f"url.{repository.as_uri()}.insteadOf", url], check=True)
            environment = dict(os.environ, CODEX_HOME=str(home),
                               GIT_CONFIG_GLOBAL=str(git_config), GIT_CONFIG_NOSYSTEM="1")

            def git(*args):
                return subprocess.run(["git", *args], cwd=repository, env=environment,
                                      text=True, capture_output=True, check=True)

            def native(*args, success=True):
                result = subprocess.run([CODEX, "plugin", *args, "--json"], cwd=root,
                                        env=environment, text=True, capture_output=True)
                self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
                return json.loads(result.stdout) if success else result

            def release(version, broken=False):
                manifest = json.loads((package / ".codex-plugin/plugin.json").read_text())
                manifest["version"] = version
                manifest["name"] = "wrong-name" if broken else "delm"
                package_release.write_json(package / ".codex-plugin/plugin.json", manifest)
                executable(package / "bin/delm", f"#!/bin/sh\necho 'delm {version}'\n")
                package_release.write_json(repository / ".agents/plugins/marketplace.json", {
                    "name": "delm", "plugins": [{"name": "delm", "source": {
                        "source": "git-subdir", "url": url, "path": "./plugins/delm",
                        "ref": f"delm-plugin-v{version}"}}]})
                git("add", ".")
                git("commit", "-m", f"Release {version}")
                git("tag", f"delm-plugin-v{version}")

            git("init", "-b", "marketplace")
            git("config", "user.email", "fixture@example.invalid")
            git("config", "user.name", "DeLM fixture")
            release("0.2.1")
            native("marketplace", "add", url, "--ref", "marketplace")
            first = native("add", "delm@delm")
            first_path = Path(first["installedPath"])
            self.assertTrue((first_path / "bin/delm").is_file())
            repeated = native("add", "delm@delm")
            self.assertEqual(repeated["installedPath"], str(first_path))
            release("0.2.2")
            native("marketplace", "upgrade", "delm")
            installed = native("list", "--marketplace", "delm")["installed"]
            self.assertEqual([(entry["pluginId"], entry["version"]) for entry in installed],
                             [("delm@delm", "0.2.2")])
            upgraded_path = home / "plugins/cache/delm/delm/0.2.2"
            self.assertNotEqual(first_path, upgraded_path)
            self.assertEqual(subprocess.check_output([str(upgraded_path / "bin/delm")], text=True).strip(),
                             "delm 0.2.2")
            release("0.2.3", broken=True)
            native("marketplace", "upgrade", "delm", success=False)
            native("add", "delm@delm", success=False)
            self.assertTrue((upgraded_path / "bin/delm").exists())
            native("remove", "delm@delm")
            native("remove", "delm@delm")
            self.assertFalse(upgraded_path.exists())
            self.assertEqual(retained.read_text(), "saved result")
            self.assertEqual((home / "auth.json").read_text(), '{}\n')
            self.assertIn('model = "fixture-model"', (home / "config.toml").read_text())
            self.assertEqual(native("list", "--marketplace", "delm")["installed"], [])


if __name__ == "__main__":
    unittest.main()
