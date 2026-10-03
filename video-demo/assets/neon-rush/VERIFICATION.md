# Verification

The retained private project contains the complete game. The original repository was not opened or modified. No package installation or server is needed to play.

## Automated results

Ran in the private workspace with Node.js **v22.17.0**:

```sh
node --test tests/*.test.js
```

Final integrated result, after importing the soundtrack and controller contributions:

```text
1..25
# tests 25
# pass 25
# fail 0
# cancelled 0
# skipped 0
# todo 0
```

Native run evidence: `exec-59db825f-94ce-49ec-8153-518a9d4e4b90`, exit **0**.

| Area | What was verified |
| --- | --- |
| Physics and course — 7 checks | Consistent movement/jump arcs at 30, 60 and 144 Hz; repeated held jumps; triangle collisions that forgive empty corners; spike/gap/platform death; platform landing; clean reset; exactly one completion event. The real-physics solver finds safe jump windows at all 31 encounters and replays the entire course to 100% at all three refresh rates. |
| Music — 8 checks | The composed score is exactly 140 beats/60 seconds; all instrument layers and evolving sections exist; music only initializes after unlock; deterministic restart; pause cancels voices and delay tails; resume retains phase; the whole score schedules valid values and releases voices; stalled-frame resynchronization; independent zero-volume muting and jump/death/finish effects. |
| Controller and renderer — 10 checks | Actual classic scripts run in a VM with DOM/canvas/audio boundaries mocked. Covers silent title, Play, short keyboard and pointer taps, held input, death/retry, progress storage, Escape, focus loss, hidden-tab launch, settings and focus trapping, denied storage/audio, a complete keyboard-driven winning run, victory/replay, and desktop/portrait/landscape canvas dimensions. |

Also ran syntax checks for the JavaScript sources and an offline asset/DOM audit: **41 unique HTML IDs**, all controller DOM targets present, all **5 local stylesheet/script references** resolved, and no module imports or fetch dependencies. The stylesheet's braces and parentheses were checked while formatting it for readability.

The controller checks exposed two edge cases, which were fixed and verified: a Space tap released between animation frames now remains buffered; pausing or losing focus now saves the current best progress immediately.

## Limits of these checks

Automated behavior checks are **not** a visual or listening review. Headless Chrome startup aborted in the managed sandbox. A separate native WebKit snapshot attempt failed because launch-services/font/WebKit sandbox extensions were unavailable; its web-content process terminated (exit 4, `exec-dff97fec-b58d-48e6-961a-8fa03a5d2b05`). No browser screenshots or audible output were available to judge.

Accordingly, the actual desktop/phone CSS appearance, physical-device touch behavior, and perceptual music mix remain unverified here. The soundtrack composition, note scheduling, transport, effects routing, rendering calls, responsive world dimensions, and gameplay behavior were verified as described above. All attempted browser processes exited, temporary harnesses/caches were removed, and no preview service remains running.
