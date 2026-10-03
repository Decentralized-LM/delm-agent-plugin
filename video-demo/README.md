# DeLM for Codex: video demo

[Watch the 56-second film](renders/delm-demo.mp4).

A terminal-led product demo: install DeLM, type a request in Codex's bottom composer, follow two agents building and sharing features, then see the working result. The edit is 1920×1080 at 60 fps, with the research video's music and synchronized interface sounds.

## Open the project

Requires macOS with its Terminal font, Node 22 or newer, Python 3, FFmpeg, and Chrome. From this folder:

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

- `src/content.js`: installer command, prompt, and links.
- `src/template.html`: scene structure.
- `src/film.css`: typography, windows, and composition.
- `src/film.js`: transitions, typing, and file handoffs.
- `src/terminal-camera.js`: rigid terminal projection and alignment.
- `src/typing.json`: generated character timing shared by picture and sound.
- `src/workflow.json`: task claims, publications, imports, and verification events.
- `src/timing.json`: shared 56-second timing map for picture and sound.
- `src/features.js`: illustrated movement and soundtrack previews.
- `assets/`: Geist fonts, sound, real game source, and captured footage.
- `scripts/`: build, capture, sound mix, and export checks.
- `reference/`: installation and rendering notes.

`index.html` is generated from `src/` by `npm run build`. The render and preview commands build it automatically. [Creative direction](CREATIVE.md) explains the purpose of each shot. [Asset sources](assets/SOURCES.md) records what comes from the original DeLM session.

## What the film shows

The film opens with “DeLM for Codex” for 1.2 seconds. Installation follows, then the request, parallel work, a brief view of the integrated game, and the close. During collaboration, the shared context and task queue sit between the workers. Numbered task claims match their current work, and each window contains its own feature preview. Shared findings and attached files remain visible while labeled paths show publication and reuse. Feature previews gain jumping, obstacles, bass, and arpeggios as the code develops. The result shot carries those contributions into the real game.

The Codex interaction and feature canvases are illustrative reconstructions of real collaboration. The game footage comes from the unchanged retained DeLM-built project, captured with mouse and keyboard inputs. This is not a newly timed DeLM run, and the film presents no baseline comparison or measured speedup.

`npx delm-agent-plugin` is an approved future installer placeholder and is not published yet. Replace it with the released and tested command before a public launch. Installation, hook trust, and restarts are abridged. Current setup still requires reviewing and trusting DeLM in `/hooks` and restarting Codex; those steps are not shown.

Titles use Geist. Terminal text uses the exact SF Mono font from this Mac's Terminal app, copied into `.cache/fonts/` for local rendering and not redistributed.

The soundtrack reuses the research video's `clean-desk.mp3`, with its original tempo and approved gain. The first 56.2 seconds play with a gentle closing fade. Typing, confirmation, and file arrivals use quiet digital ticks without noise sweeps. `npm run sound` rebuilds the character timing and mix from the retained recording without generating music. [Asset sources](assets/SOURCES.md) records the source and edit.

`npm run capture` rebuilds the source recording and its six-second edit. `npm run edit-footage` rebuilds just the edit from the retained recording. `preview.html` opens the exported film without the editor.
