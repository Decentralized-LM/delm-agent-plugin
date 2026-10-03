# Demo asset sources

The Neon Rush files are an unchanged copy of the game produced by a completed DeLM session. The source, tests, README, and original verification notes are included. No run logs, account details, or control files are included. This is the actual retained output, not a separate game built for the video.

The original verification records 25 passing automated tests and explicitly records that visual and listening review were unavailable during that run. New footage can show the retained game running, but should not imply that a new DeLM run was recorded or that the original run included browser verification.

The film is 56.2 seconds, opening with “DeLM for Codex,” followed by installation, the Codex request, collaboration, the integrated game, and the closing invitation. It makes no measured speed or baseline comparison claim.

## Reconstructed interface

The installer and Codex interface are animated reconstructions. Installer status messages follow the source installer's native plugin registration steps. The `npx delm-agent-plugin` command and branded ASCII output are an approved prototype, not a published installer. Installation, hook trust, and restarts are abridged. Current setup still requires reviewing and trusting DeLM in `/hooks` and restarting Codex; those steps are not shown.

`src/features.js` draws illustrative movement and soundtrack canvases using details from the retained game. These are not captured agent previews or progress telemetry. The real game footage remains separate and unchanged. `src/timing.json` provides the shared picture and sound timing map.

## Sound

`audio/effects.wav` is copied unchanged from the existing DeLM research video and contains synthesized clicks only.

Mouse sounds use a retimed slice of the first click at 3.100 seconds. The 3.098–3.112 second slice contains the complete 8 ms click with a little silence around it. The source combines decaying 2300 Hz and 4200 Hz sine waves and has no licensed sample dependency. The old effects stem is not played as a full track.

Typing uses quiet, dry 18 ms digital harmonic ticks. Enter has a short two-tone confirmation. File handoffs are silent in transit, with one dry tick when a file is published and another when it is imported. There are no noise sweeps, air gestures, or keycap sounds. `scripts/mix-sound.py` synthesizes these sounds and aligns them through `src/timing.json`. The generated `src/typing.json` supplies the exact character-reveal times used by both picture and typing sounds.

The music is the user's approved research-video track, `Clean_Desk_Energy_Minimal_Tech_Vibe.mp3`, copied unchanged from `delm-promotion-video/assets/audio/clean-desk.mp3` into `audio/clean-desk.mp3`. The demo uses source seconds 0–56.2 at the original speed and the research mix's gain of -12.87 dB. A 15 ms entry fade prevents a click; a 1.5-second fade closes the excerpt. The recording is not looped, rearranged, or normalized.

`audio/demo-soundtrack.wav` combines the music at the approved research-video gain with the interface sounds. `audio/demo-cues.json` records the source, mix measurements, sound categories, and cue times. Rebuilding the mix does not generate new music.

## Fonts

Geist Regular, Medium, and Semibold are copied from the research video. Their included SIL Open Font License is in `fonts/LICENSE.txt`. Titles use Geist.

Terminal text uses SF Mono Regular from `/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts/SF-Mono-Regular.otf`. The build copies that exact local font into `.cache/fonts/` for rendering. It is not included in the redistributable assets.

## Coordination shown in the video

The following events occurred in the retained game's DeLM shared context, in this order:

1. Agent 1 published the working physics and level. Agent 2 imported them while building the soundtrack.
2. Agent 2 published `js/audio.js` and audio tests. Agent 1 imported them and published the working game interface.
3. Agent 2 imported the interface, ran controller tests, and found that pause or focus loss did not save best progress.
4. Agent 2 published the correction and controller tests. Agent 1 imported those files and completed the integrated checks with 25 tests passing.

The queue summarizes actual claimed tasks: Agent 1 owned physics, the authored level, and gameplay tests; Agent 2 owned the procedural soundtrack and sound engine, then controller integration checks. The brief result shot carries both contributions into the game.

Short original excerpts suitable for the animated reconstruction:

- Agent 2: “First working audio module published.”
- Agent 2: “Running actual controller and renderer through VM harness now.”
- Agent 2: “pause/focus-loss does not save current best”
- Agent 1: “I will import your final game.js/tests and run integration.”

Editing these into shorter labels is fine if they are presented as a summary of the collaboration, rather than as a verbatim terminal transcript. Shared code moves through the shared context; it is not direct peer filesystem access.

## Recorded game footage

`footage/neon-rush.mp4` is a 12-second recording of that unchanged game in a separate headless Chrome instance, captured at 1920 × 1080 and 60 fps by `scripts/capture-game.mjs`. The clip contains no audio. The browser clock advances for every frame, and real mouse and keyboard events control the game. The solver chooses a beatable input route from the game's existing test strategy; it does not change the physics, score, level, or controller.

The title is visible from 0–2 seconds. Play is clicked at 2 seconds. Successful jumps appear at 4.233, 5.950, 7.717, and 10.467 seconds. Escape pauses at 8 seconds and resumes at 9 seconds. The recording ends on attempt 1 with 15% progress and no browser errors. Exact timings are recorded in `footage/capture.json`.

The copied source's 25 tests pass again. The only added game-directory file is `package.json`, which keeps those original CommonJS tests working inside this video's ESM project. Original game source files remain unchanged.

The delivery uses source seconds 1.4–7.4. `npm run edit-footage` recreates that six-second clip; `npm run capture` recreates both the source recording and the edit. The edit bounds are retained in `footage/capture.json`.
