# Neon Rush

A one-button precision platformer: one cube, one minute, 140 beats. A hand-designed course, electric cyan and magenta skyline, and an original procedural synthwave score. No packages, downloads, external fonts, or build step.

## Play

Open **`index.html`** in a current browser and press **LET’S RUSH**. It works directly over `file://`; all assets are generated locally. On phones, use a browser that can open local HTML, or serve the same folder with any static web server. Landscape gives a larger view, and portrait is supported.

| Control | Action |
| --- | --- |
| Space / ↑ / left click / tap the game | Jump; hold to jump again on landing |
| Escape / pause button | Pause or resume |
| R | Immediately restart the current run |
| Speaker button | Separate music and effects volume controls |

Your cube moves automatically and rotates in the air. Magenta spikes and open gaps are hazards; cyan platform tops are safe. Death starts another attempt after a short 270 ms burst of particles. Reach 100% for the finish screen and replay. Leaving the tab pauses the run; resume deliberately when you return.

The best percentage and volume settings are saved in local storage when the browser permits it. Attempts count within the current play session. Opening sound settings during a run pauses it; close settings, then resume.

## Music

**Afterglow Circuit** is an original 35-bar composition at 140 BPM in F♯ minor: four-on-the-floor kick, claps, hats, syncopated bass, chord pads, alternating arpeggios, an eight-bar lead hook, and a rising final section. Musical sections and level phases meet at beats **0, 32, 64, 96, and 124**, with the finish at beat **140** (60 seconds).

Web Audio starts after Play, stops on pause, follows the simulation clock, and begins identically with every attempt. Short jump, death, and finish sounds use the independent effects channel. Headphones recommended; either slider can mute its channel completely.

## Source and checks

- `js/core.js` — authored level, 120 Hz fixed-step movement, jump buffering/edge grace, triangle collisions, restart and completion.
- `js/renderer.js` — Canvas scenery, cube, obstacles, beat pulses, particles and trail; resolution adapts to screen size.
- `js/audio.js` — composed score, synthesis, effects, and synchronized audio transport.
- `js/game.js` — input, menus, HUD, pause, settings and persistence.
- `index.html` / `styles.css` — accessible menus and responsive layout. Reduced-motion preferences tone down impacts and decoration.

Run the dependency-free checks with **Node.js 18+** (Node is only needed for tests):

```sh
node --test tests/*.test.js
```

The checks exercise real physics and collisions, held jumps, reset, one-shot completion, a complete winning replay at 30/60/144 Hz, audio score structure, restart/pause synchronization and independent muting. The course solver searches actual simulation timing windows for every encounter, then replays a continuous winning run. DOM/audio mocks check behavior without claiming a browser listening or visual review. See [verification notes](VERIFICATION.md) for the actual run results and limitations.
