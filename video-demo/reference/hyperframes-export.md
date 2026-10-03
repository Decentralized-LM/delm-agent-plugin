# Hyperframes export notes

Verified against installed Hyperframes 0.8.114 and current Context7 `/heygen-com/hyperframes` documentation. Research only; no browser or renderer was started for these notes.

## Local scripts

Local classic `<script src>` files are supported. Keep assets inside this project and use stable relative filenames. A clean arrangement is:

```html
<body>
  <!-- the entire composition DOM -->
  <script src="assets/vendor/gsap.min.js"></script>
  <script src="scripts/composition.js"></script>
</body>
```

Important: the installed bundler concatenates every local classic script and inserts the result where the first local script appeared. Do not put GSAP in the head and local DOM-dependent animation code at the bottom. In exported HTML that animation code would move into the head with GSAP. Put both at the end of the body, or keep the animation code inline at the body end.

Keep `gsap` in the vendor filename. The static detector recognizes GSAP using script source names. Avoid modules, async/defer loading, CDN scripts, fetched assets, external fonts, or runtime imports for this self-contained composition. The existing installed GSAP file can be copied from `node_modules/gsap/dist/gsap.min.js` into `assets/vendor/gsap.min.js`.

Create a paused timeline and register the exact composition ID:

```js
const tl = gsap.timeline({ paused: true });
// Add seekable animation here.
window.__timelines = window.__timelines || {};
window.__timelines['delm-demo'] = tl;
```

The root must have matching `data-composition-id="delm-demo"` plus duration, width, and height. Do not call `.play()` for render-critical animation. Do not depend on wall-clock timers, non-seeded randomness, or asynchronous DOM edits. Use the timeline's explicit time to compute any custom canvas rendering.

Source: installed `dist/chunk-OCNCK5FQ.js`, local JS bundling around lines 2680-2725; `dist/chunk-YJEGT5L6.js` GSAP detection around line 8474. Current GSAP adapter docs: https://github.com/heygen-com/hyperframes/blob/main/skills/hyperframes-animation/adapters/gsap.md.

## Commands

Run from `video-demo`. These variables disable update requests and telemetry without changing global settings:

```sh
export HYPERFRAMES_NO_UPDATE_CHECK=1
export HYPERFRAMES_NO_TELEMETRY=1
export HYPERFRAMES_BROWSER_PATH='/absolute/path/to/an/existing/Chrome/executable'
```

Prefer an existing qualified headless-shell binary. `HYPERFRAMES_BROWSER_PATH` is supported by both browser discovery and the renderer. The currently installed Playwright package points to a missing Chromium 1243 binary, so do not use `chromium.executablePath()` without checking it exists. A system Chrome executable also works, but browser version changes can affect pixels. Do not start a real user profile.

```sh
./node_modules/.bin/hyperframes lint
./node_modules/.bin/hyperframes render --fps 30 --quality draft --workers 2 --output renders/delm-demo-review.mp4
./node_modules/.bin/hyperframes render --fps 60 --quality delivery --workers 4 --output renders/delm-demo.mp4
```

Use the installed local executable to avoid resolving a newer CLI via npx. `delivery` maps to high quality; `looks` maps to standard with CRF 16. Explicit paths, locked dependencies, local font files, and deterministic timelines make rerenders reproducible in the same environment. Byte-identical output across machines requires the documented pinned Docker rendering environment; do not imply local export guarantees that.

For a Studio handoff:

```sh
./node_modules/.bin/hyperframes preview --background --no-open --port 3017
```

The Studio route is `http://localhost:3017/#project/video-demo`, not just `/`. Check the actual emitted port/URL. A preview server is useful for editing but is not a completed MP4.

Do not run `hyperframes feedback`, `publish`, or any command that uploads the private composition. Upstream documentation recommends feedback, but this task has no authorization to send it.
