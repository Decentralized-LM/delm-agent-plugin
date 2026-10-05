"""Read-only source-install preflight; never touch a live run or its evidence."""

import hashlib
import json
import os
from pathlib import Path
import re
import stat


RUN_ID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", re.I)
TERMINAL = {"complete", "delivered", "stopped", "delivery_conflict"}


def record(path, max_bytes=16 * 1024 * 1024):
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except FileNotFoundError:
        return None
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > max_bytes:
            raise ValueError("Unsupported run record")
        value = json.load(stream)
        if not isinstance(value, dict):
            raise ValueError("Invalid run record")
        return value


def directory(path):
    if not path.is_dir() or path.is_symlink():
        raise ValueError("Run storage must contain real directories")


def verified_recovery(run):
    root = run / "workspace/recovery"
    if not root.exists():
        return False
    directory(root)
    bundle = record(root / "complete.json", 64 * 1024 * 1024)
    if not bundle or bundle.get("version") != 1 or not isinstance(bundle.get("original"), str) or not Path(bundle["original"]).is_absolute() or not isinstance(bundle.get("workers"), list) or len(bundle["workers"]) > 2:
        return False
    verified = set()
    workers = set()
    count = 0
    for worker in bundle["workers"]:
        if not isinstance(worker, dict) or type(worker.get("worker")) is not int or worker["worker"] not in (0, 1) or worker["worker"] in workers or not isinstance(worker.get("changes"), dict):
            return False
        workers.add(worker["worker"])
        for relative, pair in worker["changes"].items():
            count += 1
            parts = relative.split("/")
            if count > 1000000 or "\0" in relative or len(parts) > 128 or any(p.lower() in ("", ".", "..", ".git") for p in parts) or not isinstance(pair, list) or len(pair) != 2 or pair == [None, None]:
                return False
            for entry in pair:
                if entry is None:
                    continue
                if not isinstance(entry, dict) or entry.get("kind") not in ("file", "directory", "symlink"):
                    return False
                if (any(type(entry.get(key)) is not int or entry[key] < 0 for key in ("size", "mode", "xattrs_bytes", "flags"))
                        or entry["mode"] > 0o7777 or entry["flags"] > 0xffffffff
                        or any(not isinstance(entry.get(key), str) for key in ("xattrs_sha256", "acl_sha256"))
                        or any(entry.get(key) is not None and not isinstance(entry[key], str) for key in ("sha256", "link_target"))
                        or entry["kind"] == "symlink" and not isinstance(entry.get("link_target"), str)):
                    return False
                if entry["kind"] != "file":
                    continue
                digest, size = entry.get("sha256"), entry.get("size")
                if not isinstance(digest, str) or not re.fullmatch(r"[a-f0-9]{64}", digest) or type(size) is not int or size < 0:
                    return False
                if (digest, size) in verified:
                    continue
                fd = os.open(root / digest, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
                with os.fdopen(fd, "rb") as stream:
                    info = os.fstat(stream.fileno())
                    if not stat.S_ISREG(info.st_mode) or info.st_size != size:
                        return False
                    observed = hashlib.sha256()
                    for block in iter(lambda: stream.read(1024 * 1024), b""):
                        observed.update(block)
                    if observed.hexdigest() != digest:
                        return False
                verified.add((digest, size))
    return True


def assert_maintenance_safe(host, home=None, package=None):
    root = (Path(home) if home is not None else Path.home()) / "Library/Application Support/DeLM"
    try:
        if not os.path.lexists(root):
            return
        directory(root)
        runs = root / "runs"
        if not os.path.lexists(runs):
            return
        directory(runs)
        entries = list(runs.iterdir())
        if len(entries) > 16384:
            raise ValueError("Too many runs")
        for run in entries:
            if not RUN_ID.fullmatch(run.name):
                continue
            directory(run)
            claude, codex = record(run / "claude.json"), record(run / "run.json")
            if claude is not None and codex is not None:
                raise ValueError("Conflicting host records")
            if claude is not None and host == "codex" or codex is not None and host == "claude":
                continue
            value = claude if claude is not None else codex
            if package is not None and claude is not None and isinstance(claude.get("package_root"), str):
                if not Path(claude["package_root"]).is_absolute():
                    raise ValueError("Invalid package identity")
                if Path(claude["package_root"]).resolve() != Path(package).resolve():
                    continue
            remaining = False
            workspace = run / "workspace"
            if os.path.lexists(workspace):
                directory(workspace)
                remaining = any(p.is_symlink() or p.is_dir() and p.name not in ("delivery", "recovery") for p in workspace.iterdir())
            status = value.get("status") if value else None
            finalization = claude.get("finalization") if claude is not None else None
            shutdown_unconfirmed = finalization is not None and (
                not isinstance(finalization, dict) or finalization.get("shutdown_ack") != "confirmed")
            failed_before_start = value and status in ("preparation_failed", "startup_failed") and value.get("finished") is True and (
                claude is not None or value.get("native_started") is False and value.get("workspace_cleanup_complete") is True)
            recovered = False
            if status == "recovery_required" and not remaining:
                shutdown = record(run / "shutdown-report.json") if codex is not None else None
                quiet = value.get("finished") is True if claude is not None else shutdown and shutdown.get("ownership_resolved") is True and shutdown.get("survivors") == [] and shutdown.get("errors") == []
                recovered = quiet and verified_recovery(run)
            if not value or not (status in TERMINAL or failed_before_start or recovered) or claude is not None and value.get("finished") is not True or shutdown_unconfirmed or remaining:
                raise RuntimeError(f"DeLM run {run.name} is active or needs recovery. Stop it in its owning conversation and confirm shutdown and saved work before rebuilding, updating or removing the plugin. Saved work was not changed.")
    except (OSError, ValueError, TypeError, KeyError, AttributeError) as error:
        raise RuntimeError("Could not safely inspect DeLM run storage. Resolve its permissions or recovery state before maintenance. Saved work was not changed.") from error
