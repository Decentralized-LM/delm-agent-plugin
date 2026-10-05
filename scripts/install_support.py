"""Install/remove only this native plugin through the existing stock Codex CLI."""

import argparse
from maintenance import assert_maintenance_safe
import contextlib
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import uuid

try:
    import fcntl
except ImportError:
    # Let the platform preflight explain unsupported systems before using locks.
    fcntl = None


MARKETPLACE = "delm-local"
PLUGIN_ID = "delm@" + MARKETPLACE
SOURCE = Path(__file__).resolve().parent.parent


def preflight(operation, codex, codex_home, build=True):
    """Check prerequisites without building, registering a plugin, or reading auth."""
    if sys.platform != "darwin":
        raise RuntimeError("DeLM currently supports macOS only; Windows and Linux installation is not available yet.")
    codex = shutil.which(codex) if codex else None
    if not codex:
        raise RuntimeError("Install stock Codex CLI and make it available on PATH, then retry. "
                           "A desktop or IDE installation alone is not sufficient; DeLM does not bundle Codex.")
    codex = str(Path(codex).resolve())
    if not codex_home.is_dir():
        raise RuntimeError(f"Codex home does not exist: {codex_home}. "
                           "Run Codex once to complete setup, or set CODEX_HOME to your existing Codex home.")

    commands = [("list", "--marketplace", MARKETPLACE), ("marketplace", "list")]
    if operation == "install":
        commands += [("add", PLUGIN_ID), ("marketplace", "add", str(SOURCE))]
    else:
        commands += [("remove", PLUGIN_ID), ("marketplace", "remove", MARKETPLACE)]
    for arguments in commands:
        command = [codex, "plugin", *arguments, "--json", "--help"]
        try:
            result = subprocess.run(command, text=True, capture_output=True, timeout=15,
                                    env=dict(os.environ, CODEX_HOME=str(codex_home)))
        except (OSError, subprocess.TimeoutExpired) as error:
            raise RuntimeError("Could not check Codex's native plugin commands. "
                               "Check that `codex --help` works, then retry.") from error
        if result.returncode:
            raise RuntimeError("This Codex CLI does not support the native plugin commands DeLM requires. "
                               "Update stock Codex CLI and retry; use --codex if multiple installations are on PATH.")

    if operation == "install":
        if not shutil.which("git"):
            raise RuntimeError("Git is required. Install Git or run `xcode-select --install`, then retry.")
        if build:
            if not shutil.which("cargo"):
                raise RuntimeError("Rust/Cargo is required for a source installation. "
                                   "Install Rust and the toolchain in rust-toolchain.toml, then retry.")
            xcrun = shutil.which("xcrun")
            if not xcrun:
                raise RuntimeError("Xcode Command Line Tools are required to build DeLM. "
                                   "Run `xcode-select --install`, then retry.")
            try:
                compiler = subprocess.run([xcrun, "--find", "clang"], text=True,
                                          capture_output=True, timeout=15)
            except (OSError, subprocess.TimeoutExpired) as error:
                raise RuntimeError("Could not check Xcode Command Line Tools. "
                                   "Check that `xcrun --find clang` works, then retry.") from error
            if compiler.returncode or not compiler.stdout.strip():
                raise RuntimeError("Xcode Command Line Tools are unavailable or not selected. "
                                   "Run `xcode-select --install`, then verify `xcrun --find clang` before retrying.")
    return codex


@contextlib.contextmanager
def locked(path):
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink != 1:
            raise RuntimeError(f"Unsafe operation lock: {path}")
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        os.close(fd)


def fingerprint(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode):
            raise RuntimeError(f"Expected an ordinary file: {path}")
        digest = hashlib.sha256()
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
        return {"sha256": digest.hexdigest(), "mode": stat.S_IMODE(info.st_mode)}


def package_files(root):
    if root.is_symlink() or not root.is_dir():
        raise RuntimeError(f"Expected a real plugin directory: {root}")
    files = {}
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise RuntimeError(f"Refusing a linked plugin path: {path}")
        if not path.is_dir():
            files[str(path.relative_to(root))] = fingerprint(path)
    return files


def save_receipt(path, data):
    fd, pending = tempfile.mkstemp(prefix=path.name + ".", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            json.dump(data, stream, indent=2)
            stream.write("\n")
        os.replace(pending, path)
    finally:
        if os.path.exists(pending):
            os.unlink(pending)


class Installation:
    def __init__(self, source, codex_home, codex, cwd):
        self.source = source.resolve()
        self.codex_home = codex_home.resolve()
        self.codex = codex
        self.cwd = cwd
        receipts = self.source / ".build/installations"
        receipts.mkdir(parents=True, exist_ok=True)
        key = hashlib.sha256(str(self.codex_home).encode()).hexdigest()[:24]
        self.receipt_path = receipts / (key + ".json")
        self.identity = {"schema": 1, "source": str(self.source),
                         "codex_home": str(self.codex_home), "plugin_id": PLUGIN_ID}

    def native(self, *args):
        # Staging may take minutes after the initial preflight. Recheck at
        # activation/removal so a run started during that interval is protected.
        if args and (args[0] in {"add", "remove"} or
                     args[0] == "marketplace" and len(args) > 1 and args[1] in {"add", "remove"}):
            assert_maintenance_safe("codex")
        result = subprocess.run([self.codex, "plugin", *args, "--json"], cwd=self.cwd,
                                env=dict(os.environ, CODEX_HOME=str(self.codex_home)),
                                text=True, capture_output=True)
        if result.returncode:
            raise RuntimeError(result.stderr.strip() or result.stdout.strip() or "Native Codex plugin command failed.")
        return json.loads(result.stdout)

    def receipt(self):
        if not self.receipt_path.exists():
            return None
        fingerprint(self.receipt_path)
        data = json.loads(self.receipt_path.read_text())
        if any(data.get(key) != value for key, value in self.identity.items()):
            raise RuntimeError("The installation receipt does not match this source and CODEX_HOME.")
        return data

    def marketplace(self):
        entries = self.native("marketplace", "list")["marketplaces"]
        matches = [entry for entry in entries if entry["name"] == MARKETPLACE]
        for entry in matches:
            if Path(entry["root"]).resolve() != self.source:
                raise RuntimeError(f"Marketplace {MARKETPLACE} belongs to another source; preserving it.")
        return matches[0] if matches else None

    def installed(self, marketplace):
        if marketplace is None:
            return False
        entries = self.native("list", "--marketplace", MARKETPLACE)["installed"]
        return any(entry["pluginId"] == PLUGIN_ID for entry in entries)

    def check_cached_files(self, receipt):
        if receipt and receipt.get("installed_path"):
            root = Path(receipt["installed_path"])
            expected_parent = self.codex_home / "plugins/cache" / MARKETPLACE / "delm"
            if root.parent != expected_parent:
                raise RuntimeError("The receipt points outside this plugin's native cache.")
            if root.exists() and package_files(root) != receipt["files"]:
                raise RuntimeError(f"Installed plugin files changed; preserving them at {root}.")

    def install(self, build=True):
        assert_maintenance_safe("codex")
        receipt = self.receipt()
        marketplace = self.marketplace()
        if self.installed(marketplace) and (not receipt or receipt.get("removed")):
            raise RuntimeError(f"{PLUGIN_ID} was installed outside this installer; preserving it.")
        if receipt and receipt.get("removed"):
            receipt = None
        self.check_cached_files(receipt)
        if build:
            subprocess.run([str(self.source / "scripts/build.sh")], cwd=self.source, check=True)
        with locked(self.source / ".build/plugin-build.lock"):
            package = self.source / ".build/plugin"
            package_files(package)
            if os.path.lexists(package / "plugin.json"):
                raise RuntimeError("Rebuild DeLM before installing: the staged portable manifest disables native hooks.")
            if not (package / "hooks/hooks.json").is_file():
                raise RuntimeError("Rebuild DeLM before installing: the staged plugin is missing its lifecycle hooks.")
            outcome = self.native("marketplace", "add", str(self.source))
            owned = bool(receipt and receipt.get("owns_marketplace")) or not outcome["alreadyAdded"]
            data = {**(receipt or {}), **self.identity, "owns_marketplace": owned, "removed": False}
            # Retain the previous cache receipt if an upgrade fails before activation.
            save_receipt(self.receipt_path, data)
            outcome = self.native("add", PLUGIN_ID)
            if outcome["pluginId"] != PLUGIN_ID:
                raise RuntimeError("Native Codex returned an unexpected plugin identity.")
            installed_root = Path(outcome["installedPath"])
            data.update(installed_path=str(installed_root), files=package_files(installed_root))
            save_receipt(self.receipt_path, data)
        print(f"Installed {PLUGIN_ID} in stock Codex.")
        print("Restart Codex, open /hooks, and review and trust the DeLM hooks. "
              "Restart Codex once more, then invoke $delm:run <task>. The installer does not grant hook trust.")

    def uninstall(self):
        receipt = self.receipt()
        if receipt is None or receipt.get("removed"):
            print("No active installation owned by this checkout; nothing changed.")
            return
        assert_maintenance_safe("codex")
        marketplace = self.marketplace()
        self.check_cached_files(receipt)
        # Native removal is idempotent and works even if the marketplace disappeared.
        self.native("remove", PLUGIN_ID)
        if marketplace and receipt["owns_marketplace"]:
            self.native("marketplace", "remove", MARKETPLACE)
        receipt["removed"] = True
        save_receipt(self.receipt_path, receipt)
        print("Removed this DeLM plugin registration. Results, staged builds, and previous host installations were kept.")

    def migrate(self):
        """Retire our local registration only after the public plugin is installed."""
        receipt = self.receipt()
        if not receipt or receipt.get("removed"):
            print("No active local installation owned by this checkout; nothing changed.")
            return
        assert_maintenance_safe("codex")
        public = self.native("list", "--marketplace", "delm")["installed"]
        if not any(entry["pluginId"] == "delm@delm" for entry in public):
            raise RuntimeError("Install delm@delm before retiring delm@delm-local.")
        marketplace = self.marketplace()
        cache = self.codex_home / "plugins/cache" / MARKETPLACE / "delm"
        archive = None
        if cache.exists():
            assert_maintenance_safe("codex")
            # Preserve every version and any user modifications atomically before
            # Codex's remove command deletes its cache. No receipt hash match is
            # required here: preserving modified artifacts is the point.
            package_files(cache)
            archive_parent = self.codex_home / "delm/preserved-plugins"
            archive_parent.mkdir(parents=True, exist_ok=True)
            archive = archive_parent / ("delm-local-" + uuid.uuid4().hex)
            cache.rename(archive)
        try:
            self.native("remove", PLUGIN_ID)
        except Exception:
            if archive is not None and not cache.exists():
                archive.rename(cache)
            raise
        receipt.update(removed=True, preserved_path=str(archive) if archive else None)
        save_receipt(self.receipt_path, receipt)
        if marketplace and receipt["owns_marketplace"]:
            self.native("marketplace", "remove", MARKETPLACE)
        print(f"Retired {PLUGIN_ID}; use $delm:run from delm@delm in a new Codex session.")
        if archive:
            print(f"All previous plugin files, including modifications, were kept at {archive}.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("install", "uninstall", "migrate"))
    parser.add_argument("--codex", default=shutil.which("codex"), help="Existing stock Codex executable")
    parser.add_argument("--no-build", action="store_true", help="Install an already staged .build/plugin bundle")
    args = parser.parse_args()
    codex_home = Path(os.environ.get("CODEX_HOME", str(Path.home() / ".codex"))).expanduser().resolve()
    codex = preflight(args.operation, args.codex, codex_home, build=not args.no_build)
    (SOURCE / ".build").mkdir(exist_ok=True)
    with locked(SOURCE / ".build/plugin-install.lock"), tempfile.TemporaryDirectory(prefix="delm-plugin-cli-") as cwd:
        installation = Installation(SOURCE, codex_home, codex, cwd)
        if args.operation == "install":
            installation.install(build=not args.no_build)
        elif args.operation == "uninstall":
            installation.uninstall()
        else:
            installation.migrate()


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
