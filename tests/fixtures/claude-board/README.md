# Native board preview

This fixture loads the production renderer in Claude Code with deterministic sample data. It launches no agents or application task. The capture process uses a disposable home/configuration directory, inherits no account credentials, and points the API at an unreachable loopback address.

From the repository root:

```sh
python3 tests/fixtures/claude-board/capture.py \
  --output .validation/claude-board/wide
uv run --with pyte --with pillow python \
  tests/fixtures/claude-board/render_capture.py .validation/claude-board/wide
```

The output contains native ANSI captures, terminal-cell PNGs, an asciinema recording, and capture metadata. The PNGs preserve the captured terminal layout; they are not design mockups. Replay the `.cast` recording with an asciinema-compatible player to inspect native redraws. The fixture commands are not included in the installed plugin.

The staged preview also supports Claude's native interaction tests:

```sh
claude plugin test .validation/claude-board/wide/plugin
```

To exercise the production observer and board adapter together, first build the runtime, then capture a saved fixture through the same command entry point:

```sh
cargo build --locked
python3 tests/fixtures/claude-board/capture.py --integration \
  --output .validation/claude-board/integration \
  --commands '/delm:run Demonstrate parallel work,/board-hide,/delm-status'
```

This mode seeds a private, disposable SQLite board and lifecycle record. The shipped adapter launches the real read-only observer and displays its output. The immediate fixture launch still sends no model request and starts no worker. It tests data delivery and presentation, not an application-building run.

Pass `--runtime .build/plugin-claude/bin/delm` to capture the staged release executable instead of `target/debug/delm`. Add `--exercise-controls` to preserve an unsent prompt while testing native focus, navigation, close, and reopen. At very small terminal sizes, the first selectable control can be a collection link rather than a task row.

`/board-recovery` shows an unfinished result awaiting shutdown confirmation, including the production Retry finishing control and continued-conversation message. Its fixture action changes only sample presentation state; it cannot recover or modify a real run. The control sequence also captures Page Down and Page Up in a detail view to check native scrolling.

Use `--columns 90 --rows 34` for a narrow fullscreen terminal, `--classic` for the classic renderer, and `--theme light` for light theme. The default sequence opens the board, opens details, hides it, reopens it, and shows a result that still requires local verification. Use `--commands /board-preparing,/board-preview,/board-stopped` to inspect preparation and recovery.

For long Unicode titles and constrained heights, use `--columns 80 --rows 24 --commands /board-unicode,/board-complete,/board-stopped,/board-attention`. The native pane scrolls when its viewport cannot show the complete board.

The display uses the terminal's font. The PNG helper uses Menlo on macOS and DejaVu Sans Mono on Linux; glyph rendering depends on the local font. ANSI and `.cast` files remain the source of truth. Native host validation, visual captures, renderer tests, and observer tests establish different guarantees; a preview is not evidence that a model completed a real task.
