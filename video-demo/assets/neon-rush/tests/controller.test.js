const test = require('node:test');
const assert = require('node:assert/strict');
const { createDOM } = require('./helpers/dom.js');

function setup(options = {}) {
  let sim, audio, renderer;
  const calls = [];
  const app = createDOM({ ...options, beforeScript(filename, context) {
    if (filename !== 'js/game.js') return;
    const Simulation = context.NeonCore.Simulation;
    context.NeonCore.Simulation = class extends Simulation { constructor(...args) { super(...args); sim = this; } };
    const Audio = context.NeonAudio;
    context.NeonAudio = class extends Audio {
      constructor(...args) {
        super(...args); audio = this;
        for (const name of ['unlock', 'start', 'pause', 'resume', 'stop', 'jump', 'death', 'finish']) {
          const original = this[name].bind(this);
          this[name] = (...values) => { calls.push({ name, values }); return original(...values); };
        }
      }
    };
    const Renderer = context.NeonRenderer;
    context.NeonRenderer = class extends Renderer { constructor(...args) { super(...args); renderer = this; } };
  } });
  app.frame();
  return { ...app, sim, audio, renderer, calls,
    async start() { await app.element('play-button').fire('click'); app.frame(); },
    key(type, code, extra = {}) { return app.document.fire(type, { code, target: app.element('game-canvas'), ...extra }); }
  };
}

test('title initializes silently and Play starts the actual simulation and audio transport', async () => {
  const app = setup(); app.step(.5);
  assert.equal(app.element('title-screen').hidden, false);
  assert.equal(app.element('game-hud').hidden, true);
  assert.equal(app.audio.context, null);
  assert.equal(app.sim.elapsed, 0);
  assert.equal(app.calls.length, 0);
  await app.start(); app.step(.5);
  assert.equal(app.element('title-screen').hidden, true);
  assert.equal(app.element('game-hud').hidden, false);
  assert.equal(app.element('attempt').textContent, '01');
  assert.ok(app.sim.x > 120);
  assert.equal(app.calls.filter(call => call.name === 'unlock').length, 1);
  assert.equal(app.calls.filter(call => call.name === 'start').length, 1);
});

test('mouse/touch taps shorter than a frame are buffered, while holding repeats on landing', async () => {
  const app = setup(); await app.start();
  await app.element('game-canvas').fire('pointerdown', { button: 0, pointerId: 1 });
  await app.window.fire('pointerup'); app.frame();
  assert.ok(app.sim.vy < 0);
  await app.key('keydown', 'KeyR');
  await app.element('game-canvas').fire('pointerdown', { button: 0, pointerId: 2 });
  app.step(1.6);
  assert.ok(app.calls.filter(call => call.name === 'jump').length >= 4);
  await app.window.fire('pointercancel'); app.step(.8);
  assert.equal(app.sim.grounded, true);
});

test('Space taps shorter than a frame are also buffered', async () => {
  const app = setup(); await app.start();
  await app.key('keydown', 'Space'); await app.key('keyup', 'Space'); app.frame();
  assert.ok(app.sim.vy < 0, 'a quick keyboard tap must not disappear between animation frames');
});

test('death saves progress, plays its effect, and immediately retries with music at zero', async () => {
  const app = setup(); await app.start(); app.step(2.85);
  assert.equal(app.element('attempt').textContent, '02');
  assert.equal(app.sim.status, 'running'); assert.ok(app.sim.elapsed < .5);
  assert.ok(Number(app.storage.get('neonrush.best')) >= 4);
  const starts = app.calls.filter(call => call.name === 'start');
  assert.equal(starts.length, 2); assert.ok(starts.every(call => call.values[0] === 0));
  assert.equal(app.calls.filter(call => call.name === 'death').length, 1);
});

test('Escape pauses movement and music, clears held input, and resumes the same attempt', async () => {
  const app = setup(); await app.start(); app.step(.4);
  await app.key('keydown', 'Space'); await app.key('keydown', 'Escape');
  const elapsed = app.sim.elapsed;
  assert.equal(app.element('pause-screen').hidden, false);
  app.step(1); assert.equal(app.sim.elapsed, elapsed);
  await app.key('keydown', 'Escape'); app.step(.3);
  assert.equal(app.element('pause-screen').hidden, true);
  assert.equal(app.element('attempt').textContent, '01');
  assert.equal(app.sim.grounded, true, 'held state must not survive pausing');
  const resume = app.calls.find(call => call.name === 'resume');
  assert.equal(resume.values[0], elapsed);
  await app.element('pause-button').fire('click');
  await app.key('keydown', 'KeyR');
  assert.equal(app.element('attempt').textContent, '02');
  assert.equal(app.sim.elapsed, 0); assert.equal(app.element('pause-screen').hidden, true);
});

test('losing focus pauses and saves current best; hidden-tab launch starts safely paused', async () => {
  const app = setup(); await app.start(); app.step(1);
  await app.window.fire('blur');
  assert.equal(app.element('pause-screen').hidden, false);
  assert.ok(Number(app.storage.get('neonrush.best')) >= 1.6, 'progress is retained if the user leaves mid-attempt');
  const elapsed = app.sim.elapsed; app.step(.5); assert.equal(app.sim.elapsed, elapsed);
  await app.element('resume-button').fire('click');
  app.document.hidden = true; await app.document.fire('visibilitychange');
  assert.equal(app.element('pause-screen').hidden, false);
  const hidden = setup(); hidden.document.hidden = true; await hidden.start();
  assert.equal(hidden.element('pause-screen').hidden, false); assert.equal(hidden.sim.elapsed, 0);
});

test('settings pause the run, volumes stay independent, and controls trap keyboard focus', async () => {
  const app = setup(); await app.start(); app.step(.2);
  await app.element('settings-button').fire('click');
  assert.equal(app.element('settings-screen').hidden, false);
  assert.equal(app.element('pause-screen').hidden, false);
  const elapsed = app.sim.elapsed;
  app.element('music-volume').value = '0'; await app.element('music-volume').fire('input');
  assert.equal(app.audio.musicVolume, 0); assert.equal(app.audio.effectsVolume, .65);
  app.element('effects-volume').value = '28'; await app.element('effects-volume').fire('input');
  assert.equal(app.audio.musicVolume, 0); assert.equal(app.audio.effectsVolume, .28);
  assert.deepEqual(JSON.parse(app.storage.get('neonrush.audio')), { music: 0, effects: 28 });
  app.element('settings-done').focus(); await app.key('keydown', 'Tab');
  assert.equal(app.document.activeElement.id, 'close-settings');
  await app.key('keydown', 'Space', { target: app.element('music-volume') });
  app.step(.5); assert.equal(app.sim.elapsed, elapsed);
  await app.key('keydown', 'Escape');
  assert.equal(app.element('settings-screen').hidden, true);
  assert.equal(app.element('pause-screen').hidden, false);
});

test('denied local storage and unavailable audio do not prevent playing or retrying', async () => {
  const app = setup({ storageDenied: true }); await app.start(); app.step(2.85);
  assert.equal(app.element('attempt').textContent, '02');
  assert.equal(app.sim.status, 'running');
  assert.equal(app.element('sound-status').textContent, 'SOUND UNAVAILABLE');
  await app.element('pause-button').fire('click'); await app.element('quit-button').fire('click');
  assert.equal(app.element('title-screen').hidden, false);
});

// A recorded winning input route (120 Hz simulation steps), independently
// checked by core.test.js's encounter solver. Here the inputs pass through the
// production keyboard handler, renderer, progress HUD, save and finish flow.
const WINNING_ROUTE = [268, 474, 686, 896, 1082, 1354, 1714, 1929, 2110, 2331, 2548,
  2727, 3006, 3366, 3549, 3783, 3978, 4167, 4395, 4607, 4800, 4989, 5218, 5431, 5606,
  5835, 6049, 6240, 6452, 6634, 6872];
test('the full level finishes through keyboard input and offers a clean playable replay', async () => {
  const app = setup(); await app.start();
  let nextJump = 0;
  for (let frame = 0; frame < 3660 && app.sim.status === 'running'; frame++) {
    const jump = nextJump < WINNING_ROUTE.length && app.sim.steps >= WINNING_ROUTE[nextJump] - 1;
    if (jump) { await app.key('keydown', 'Space'); nextJump++; }
    app.frame();
    if (jump) await app.key('keyup', 'Space');
  }
  assert.equal(app.sim.status, 'complete'); assert.equal(app.sim.elapsed, 60);
  assert.equal(app.element('victory-screen').hidden, false);
  assert.equal(app.element('finish-attempts').textContent, '01');
  assert.equal(app.element('progress-bar').getAttribute('aria-valuenow'), '100');
  assert.equal(app.storage.get('neonrush.best'), '100');
  assert.equal(app.calls.filter(call => call.name === 'finish').length, 1);
  assert.equal(app.calls.filter(call => call.name === 'death').length, 0);
  await app.element('replay-button').fire('click'); app.step(.2);
  assert.equal(app.element('victory-screen').hidden, true);
  assert.equal(app.sim.status, 'running'); assert.ok(app.sim.elapsed < .3);
  assert.equal(app.element('attempt').textContent, '01'); assert.equal(app.storage.get('neonrush.best'), '100');
});

test('renderer accepts desktop, portrait, and landscape sizes with visible runway', async () => {
  const app = setup(); await app.start();
  for (const [width, height] of [[1200, 621], [347, 560], [816, 290]]) {
    const canvas = app.element('game-canvas'); canvas.clientWidth = width; canvas.clientHeight = height;
    await app.window.fire('resize'); app.step(.1);
    assert.equal(app.renderer.width, width); assert.equal(app.renderer.height, height);
    assert.ok(Number.isFinite(app.renderer.scale) && app.renderer.scale > 0);
    assert.ok((width - app.renderer.anchor) / app.renderer.scale > 450);
  }
});
