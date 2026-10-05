# DeLM for Claude Code and Codex: product film

A 55-second film built around one request and a real result. The viewer should learn that DeLM works in both Claude Code and Codex, see how to start it, and watch two agents divide the work, build side by side, and share their progress through the DeLM board.

## The edit

| Time | Picture | What it communicates |
| --- | --- | --- |
| 0–3.4 s | “DeLM for Claude Code” in orange with Clawd waving on the right. The name turns upward once to “Codex” with the Codex pet waving. The installer command sits beneath. | DeLM is for both hosts. The opening also serves as the thumbnail. |
| 3.3–10.3 s | A dark terminal settles square. The installer is typed, the host choice is Both, DeLM installs in Claude Code and then Codex, and the installer confirms it is ready in both. `claude` is typed. | One command installs DeLM everywhere. |
| 10.3–19.4 s | The window grows into Claude Code. `/delm:` opens the command menu, `/delm:run` completes, and the Neon Rush request is typed and sent. | DeLM starts from the normal prompt. |
| 19.4–22 s | The DeLM board opens beside the conversation and the first tasks appear. The camera holds on the whole window. | The board lives inside Claude Code. |
| 22–38.3 s | The camera moves in and both agents appear side by side under the conversation, each editing its own part of the game with a live preview while the board updates on the right. Both claim their first task at the same moment; after that, each share, import, and claim happens on its own beat, labeled with what moves (“Share core.js”, “Import core.js”, “Claim #4”). | Agents claim work in parallel and build on each other's contributions. |
| 38.3–40.9 s | The agents finish and Claude reports that Neon Rush is ready, with the board showing “Changes applied.” | The result lands in the project. |
| 40.9–48.2 s | The camera pushes into Claude's message and the game opens out of it, large against a synthwave night. Play is clicked. As the cube makes its first jump, a callout credits Agent 1 with the jump physics and level; a second points to the 140 BPM readout for Agent 2's soundtrack. | The request became a working game, built by both agents. |
| 48.2–55.2 s | The original closing: “Build your next idea.” rises, “Faster with DeLM.” types on, and `/delm:run` appears. Clawd and the Codex pet hop onto the command and wave. | How to start, with both hosts in view. |

## Direction

The title, game, and close sit on warm white paper with dark ink and DeLM blue. The terminal is dark, matching Claude Code's native dark theme, so the board reads exactly as it does on screen. Board colors, layout, and labels follow a native 120-column capture; see [the Claude Code palette](reference/claude-ui.md). Claude Code's name uses Anthropic's Claude orange, `#D97757`.

The two mascots are vector art in one style: dark outlines, light from the upper left, glossy highlights, and soft ground shadows. The Codex pet keeps its cloud head, screen face, and prompt marks. Clawd keeps Claude Code's wide body, side claws, and four legs. Both wave, blink, and breathe.

Each agent pane shows its current task, its latest tool call in Claude Code's style, the code it is writing, and a live preview of its feature. Routes run on separate tracks for each agent and end exactly where the board row changes. Only the opening claims happen together; everything after is staggered so one change reads at a time. Changed rows glow briefly so the eye finds each update.

The closing keeps the original layout, timing, and typing; only the command changes from `$delm:run` to `/delm:run`, and the mascots stand on it without moving anything.

The sound design accompanies the picture: layered key strokes for every typed line, deeper Enter keys, a ratchet for the title wheel, soft air for camera moves, bell plucks for claims, rising chimes for shared work and its imports, a chord when changes are applied, and game sounds on the cube's jumps. A short stereo room ties the effects together. The mix is normalized to −16 LUFS with peaks below −1.5 dBTP. There is no voiceover; the film makes sense muted.

## Production

Hyperframes renders the HTML, CSS, and GSAP timeline at 3840×2160, 60 fps. Every frame is computed from the timeline's time, so any frame renders identically when sought directly. `src/timing.json` drives picture and sound. FFmpeg prepares the sound and validates the MP4.
