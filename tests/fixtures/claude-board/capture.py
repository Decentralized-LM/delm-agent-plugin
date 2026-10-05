"""Capture the shipped renderer in a disposable native Claude terminal.

No application task is launched. The immediate preview commands use sample data;
the API endpoint is an unreachable loopback address and no account credentials
are inherited. Requires macOS/Linux, Python 3, and an installed Claude executable.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import struct
import termios
import time


def capture(args):
    fixture = Path(__file__).resolve().parent
    repo = fixture.parents[2]
    root = Path(args.output).resolve()
    root.mkdir(parents=True, exist_ok=True)
    plugin = root / "plugin"
    hooks = plugin / "hooks"
    hooks.mkdir(parents=True, exist_ok=True)
    (plugin / ".claude-plugin").mkdir(exist_ok=True)
    (plugin / ".claude-plugin/plugin.json").write_text(json.dumps({
        "name": "delm" if args.integration else "delm-board-preview", "version": "0.1.0",
        "description": "Local native presentation fixture with sample data",
        "author": {"name": "DeLM"},
    }))
    (hooks / "hooks.json").write_text(json.dumps({"modules": ["./integration.js" if args.integration else "./preview.js"]}))
    for name in ["preview.js", "states.js"]:
        shutil.copy2(fixture / name, hooks / name)
    (plugin / "tests").mkdir(exist_ok=True)
    shutil.copy2(fixture / "native.test.ts", plugin / "tests/native.test.ts")
    shutil.copy2(repo / "hosts/claude/hooks/board-render.js", hooks / "board-render.js")
    if args.integration:
        shutil.copy2(fixture / "integration.js", hooks / "integration.js")
        shutil.copy2(repo / "hosts/claude/hooks/board-view.js", hooks / "board-view.js")
        (plugin / "bin").mkdir(exist_ok=True)
        shutil.copy2(repo / "scripts/test_claude_board.py", plugin / "bin/seed.py")
        runtime = Path(args.runtime).resolve() if args.runtime else repo / "target/debug/delm"
        shutil.copy2(runtime, plugin / "bin/delm")
        (plugin / "skills/run").mkdir(parents=True, exist_ok=True)
        (plugin / "skills/run/SKILL.md").write_text("---\nname: run\ndescription: Local sample board only\ndisable-model-invocation: true\n---\nThis command is handled immediately by the local fixture.\n")
    config = root / "config"
    home = root / "home"
    config.mkdir(exist_ok=True)
    home.mkdir(exist_ok=True)
    (config / ".claude.json").write_text(json.dumps({
        "hasCompletedOnboarding": True, "hasAcknowledgedCostThreshold": True,
        "theme": args.theme,
        "customApiKeyResponses": {"approved": ["sk-ant-local-ui-no-model-call"[-20:]], "rejected": []},
        "projects": {str(root): {"hasTrustDialogAccepted": True, "hasCompletedProjectOnboarding": True}},
    }))
    (config / "settings.json").write_text(json.dumps({"skipDangerousModePermissionPrompt": True}))
    env = {
        "HOME": str(home), "CLAUDE_CONFIG_DIR": str(config), "PATH": os.environ.get("PATH", ""),
        "LANG": "en_US.UTF-8", "TERM": "xterm-256color", "COLORTERM": "truecolor",
        "ANTHROPIC_API_KEY": "sk-ant-local-ui-no-model-call", "ANTHROPIC_BASE_URL": "http://127.0.0.1:9",
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "DISABLE_AUTOUPDATER": "1",
        "CLAUDE_CODE_NO_FLICKER": "0" if args.classic else "1",
        "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN": "1" if args.classic else "0",
    }
    executable = shutil.which(args.claude)
    if not executable:
        raise SystemExit("Claude executable was not found")
    argv = [executable, "--setting-sources", "", "--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}',
            "--permission-mode", "manual", "--plugin-dir", str(plugin)]
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(root)
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", args.rows, args.columns, 0, 0))
        os.execve(executable, argv, env)
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", args.rows, args.columns, 0, 0))
    data = bytearray()
    events = []
    started = time.monotonic()

    def read(seconds):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            if select.select([master], [], [], max(0, min(.1, until - time.monotonic())))[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                data.extend(chunk)
                events.append([round(time.monotonic() - started, 6), "o", chunk.decode("utf-8", "replace")])

    try:
        read(5)
        for index, command in enumerate(args.commands.split(",")):
            os.write(master, (command + "\r").encode())
            read(2)
            (root / f"{index:02d}-{command.lstrip('/')}.ansi").write_bytes(data)
        if args.exercise_controls:
            for label, keys in [
                ("draft", b"draft follow-up"),
                ("focus", b"\x18\t"),
                ("select-task", b"\t"),
                ("task-detail", b"\r"),
                ("page-down", b"\x1b[6~"),
                ("page-up", b"\x1b[5~"),
                ("back", b"\r"),
                ("hide", b"\x18x"),
                ("compact-focus", b"\x18\t"),
                ("reopen", b"\r"),
            ]:
                os.write(master, keys)
                read(1)
                (root / f"control-{label}.ansi").write_bytes(data)
        (root / "recording.cast").write_text("\n".join(json.dumps(value) for value in [
            {"version": 2, "width": args.columns, "height": args.rows,
             "title": "DeLM native board · deterministic sample data", "env": {"TERM": "xterm-256color"}}, *events]) + "\n")
        (root / "capture.json").write_text(json.dumps({
            "columns": args.columns, "rows": args.rows, "commands": args.commands.split(","),
            "theme": args.theme, "classic": args.classic, "sample_data": True, "real_observer": args.integration,
            "exercise_controls": args.exercise_controls,
            "model_calls": 0, "api_endpoint": "unreachable loopback", "production_renderer": "hosts/claude/hooks/board-render.js",
            "source_sha256": {str(path.relative_to(plugin)): hashlib.sha256(path.read_bytes()).hexdigest()
                              for path in [hooks / "board-render.js", *([hooks / "board-view.js", plugin / "bin/delm"] if args.integration else [])]},
        }, indent=2) + "\n")
    finally:
        try:
            os.write(master, b"\x03\x03")
            read(.2)
            os.kill(pid, signal.SIGTERM)
        except (OSError, ProcessLookupError):
            pass
        deadline = time.monotonic() + 1
        while time.monotonic() < deadline:
            if os.waitpid(pid, os.WNOHANG)[0]:
                break
            time.sleep(.05)
        else:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(pid, os.WNOHANG)
        os.close(master)
    print(root)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True)
    parser.add_argument("--claude", default="claude")
    parser.add_argument("--columns", type=int, default=120)
    parser.add_argument("--rows", type=int, default=40)
    parser.add_argument("--theme", choices=["dark", "light"], default="dark")
    parser.add_argument("--classic", action="store_true")
    parser.add_argument("--integration", action="store_true", help="Use the real passive observer; build the runtime first")
    parser.add_argument("--runtime", help="Runtime executable for integration capture; defaults to target/debug/delm")
    parser.add_argument("--exercise-controls", action="store_true", help="Keep draft input while exercising native focus, details, close and reopen")
    parser.add_argument("--commands", default="/board-preview,/board-details,/board-hide,/board-preview,/board-complete")
    capture(parser.parse_args())
