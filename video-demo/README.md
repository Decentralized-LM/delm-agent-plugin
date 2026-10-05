# DeLM for Claude Code and Codex: video demo

[Watch the 55-second film](renders/delm-demo.mp4).

The title introduces DeLM for Claude Code, then turns to Codex. One installer sets up both. The rest of the film runs in Claude Code: one `/delm:run` request opens the DeLM board, two agents work side by side and share their progress through it, and the finished Neon Rush game plays. The film renders at 3840×2160 and 60 fps, with the research video's music and a synchronized sound design.

## Open the project

Requires macOS with its Terminal font, Node 22 or newer, Python 3 with NumPy and SciPy, FFmpeg, and Chrome. From this folder:

```sh
npm ci
npm run preview
```

Open the Studio address printed in the terminal to play and scrub the edit. After editing, rebuild the sound before exporting so its cues match the picture:

```sh
npm run sound
npm run render
npm run review
```

`HYPERFRAMES_BROWSER_PATH` can select another Chrome-compatible executable. The wrapper uses installed Chrome on macOS and disables Hyperframes telemetry and update checks.

## Edit

- `src/content.js`: installer command, prompt, Claude Code header, replies, and task names.
- `src/timing.json`: every scene, board change, agent step, route, and camera move, shared by picture and sound.
- `src/template.html`: scene structure.
- `src/film.css`: typography, windows, and the terminal palette.
- `src/film.js`: title wheel, installation, transitions, camera, closing, and mascots.
- `src/claude.js`: Claude Code, the DeLM board, the two agents, and the routes between them, on a 129 × 30 character grid.
- `src/features.js`: the agents' movement and soundtrack previews.
- `src/typing.json`: generated character timing shared by picture and sound.
- `assets/pets/`: Clawd and the Codex pet as vector art.
- `assets/`: Geist fonts, sound, the Neon Rush game, and its footage.
- `reference/`: the Claude Code palette and rendering notes.

`index.html` is generated from `src/` by `npm run build`. The render and preview commands build it automatically. [Creative direction](CREATIVE.md) explains each shot. [Asset sources](assets/SOURCES.md) records where each asset comes from.

## What the film shows

“DeLM for” stays still while the host name turns once like a wheel: Claude Code in orange with Clawd waving, then Codex with the Codex pet waving. The installer installs DeLM in Claude Code and Codex and confirms it is ready in both. The same terminal window grows into Claude Code, where `/delm:run` is chosen from the command menu and the Neon Rush request is sent.

The DeLM board opens beside the conversation. The camera moves in as both agents appear side by side, each editing its own part of the game with a live preview. Both agents claim their first task at the same moment; after that, each share, import, and claim travels between the agents and the board on its own beat, and every board change follows the route that caused it. When the work is applied, Claude reports that Neon Rush is ready, and the game opens out of that message. Callouts credit each agent's part as the game plays, and the film closes on `/delm:run` with both mascots standing on the command.

Titles use Geist. Terminal text uses the exact SF Mono font from this Mac's Terminal app, copied into `.cache/fonts/` for local rendering and not redistributed.

`npm run capture` rebuilds the game recording and its six-second edit. `npm run edit-footage` rebuilds just the edit. `preview.html` opens the exported film without the editor.
