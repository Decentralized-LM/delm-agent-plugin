# Demo asset sources

## Neon Rush

The Neon Rush files are an unchanged copy of the game produced by a completed DeLM session, with its source, tests, README, and verification notes. Its 25 automated tests pass. The only added game-directory file is `package.json`, which keeps the original CommonJS tests working inside this video's ESM project.

`footage/neon-rush.mp4` is a 12-second recording of the game in headless Chrome at 1920 × 1080 and 60 fps, made by `scripts/capture-game.mjs`. Real mouse and keyboard events control the game. Play is clicked at 2 seconds, and successful jumps follow. The film uses source seconds 1.4–7.4 as `footage/neon-rush-edit.mp4`. Exact timings are recorded in `footage/capture.json`.

## Mascots

`pets/codex.svg` is a vector drawing of the Codex pet, following the idle frame of its built-in spritesheet: cloud head, screen face with its prompt marks, torso mark, arms, and legs, in the same colors.

`pets/clawd.svg` is a vector drawing of Claude Code's terminal mascot in the same style, using Anthropic's Claude orange, `#D97757`. The Claude Code header inside the terminal uses the mascot exactly as Claude Code draws it.

## Sound

`audio/effects.wav` is copied unchanged from the DeLM research video and contains synthesized clicks only. The Play click uses its 3.098–3.112 second slice.

`scripts/mix-sound.py` synthesizes every other effect: layered key strokes (switch click, keycap body, and bottom-out), Enter keys, the title-wheel ratchet, filtered air for camera moves, bell plucks and chimes for board activity, and pops for the mascots. A short stereo room is applied to the effects. Cues come from `src/timing.json` and the generated `src/typing.json`.

The music is the research video's track, `audio/clean-desk.mp3`, at its original speed with a 15 ms entry fade and a 1.5-second closing fade. `audio/demo-soundtrack.wav` combines the music and effects and is normalized to −16 LUFS with true peaks below −1.5 dBTP. `audio/demo-cues.json` records every cue and the mix measurements.

## Fonts

Geist Regular, Medium, and Semibold are copied from the research video, with their SIL Open Font License in `fonts/LICENSE.txt`.

Terminal text uses SF Mono Regular and Bold from `/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts/`. The build copies them into `.cache/fonts/` for rendering. They are not included in the redistributable assets.
