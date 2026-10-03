# Task board example

This small browser application uses plain JavaScript and requires no package installation. Node.js 20 or newer runs its tests.

From the DeLM source directory, create a fresh project:

```bash
DELM_SOURCE="$PWD"
DELM_DEMO=$(mktemp -d "$HOME/delm-demo.XXXXXX")
cp -R "$DELM_SOURCE/examples/task-board/." "$DELM_DEMO/"
git -C "$DELM_DEMO" init -q --template=
git -C "$DELM_DEMO" add .
git -C "$DELM_DEMO" -c commit.gpgsign=false -c core.hooksPath=/dev/null -c user.name='DeLM Demo' -c user.email='demo@localhost' commit -qm 'Create demo project'
codex -C "$DELM_DEMO"
```

Enter:

```text
$delm:run Add task filtering and persistence to this task board. Keep its existing layout, verify the behavior, and explain the result.
```

When DeLM finishes, open the retained project linked in its response. The demo you started from remains unchanged.

Run `node --test task.test.mjs` for the existing model tests. Open `index.html` in a browser to use the board.
