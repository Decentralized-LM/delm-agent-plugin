import { chromium } from 'playwright';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(import.meta.url);
const { Simulation, level, constants } = require('../assets/neon-rush/js/core.js');
const output = resolve(root, 'assets/footage');
const fps = 60;
const duration = 12;
const edit = { sourceStartSeconds: 1.4, durationSeconds: 6, file: 'neon-rush-edit.mp4' };
function createEdit() {
  const result = spawnSync('ffmpeg', [
    '-v', 'error', '-y', '-ss', String(edit.sourceStartSeconds),
    '-i', resolve(output, 'neon-rush.mp4'), '-t', String(edit.durationSeconds),
    '-an', '-c:v', 'libx264', '-preset', 'fast', '-crf', '17',
    '-pix_fmt', 'yuv420p', '-movflags', '+faststart', resolve(output, edit.file),
  ], { stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error('Game footage edit failed.');
}
if (process.argv.includes('--edit-only')) {
  createEdit();
  process.exit(0);
}

// Find a legitimate keyboard route using the shipped physics and collision model.
// This reproduces the retained project's test solver; it does not alter the game.
function jumpSchedule() {
  const encounters = [
    ...level.spikes.filter((s, i, a) => !a.some((p, j) => j < i && s.x - p.x === 30)),
    ...level.gaps, ...level.platforms,
  ].sort((a, b) => a.x - b.x);
  let sim = new Simulation();
  const schedule = [];
  for (const hazard of encounters) {
    const target = Math.min(hazard.x + 290, level.length);
    const valid = [];
    for (let distance = -12; distance <= 155; distance += 2) {
      const clone = Object.assign(Object.create(Object.getPrototypeOf(sim)), sim, { events: [...sim.events] });
      let jumpAt = null;
      while (clone.x < target && clone.status === 'running') {
        const held = jumpAt === null && clone.x + constants.SIZE >= hazard.x - distance;
        if (held) jumpAt = clone.steps;
        clone.advance(constants.STEP, held);
      }
      if (clone.status !== 'dead' && jumpAt !== null) valid.push({ clone, jumpAt });
    }
    if (!valid.length) throw new Error(`No keyboard route past ${hazard.x}`);
    const chosen = valid[Math.floor(valid.length / 2)];
    schedule.push(chosen.jumpAt * constants.STEP);
    sim = chosen.clone;
  }
  return schedule;
}

await mkdir(output, { recursive: true });
const browser = await chromium.launch({
  executablePath: process.env.CHROME_PATH || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  headless: true,
  args: ['--mute-audio', '--hide-scrollbars'],
});
let encoder;
try {
  const page = await browser.newPage({ viewport: { width: 1920, height: 1080 }, deviceScaleFactor: 1 });
  const browserErrors = [];
  page.on('pageerror', error => browserErrors.push(error.message));
  const epoch = new Date('2026-01-01T00:00:00Z');
  await page.clock.install({ time: epoch });
  await page.goto(pathToFileURL(resolve(root, 'assets/neon-rush/index.html')).href);
  await page.clock.pauseAt(new Date(epoch.getTime() + 1000));

  encoder = spawn('ffmpeg', [
    '-hide_banner', '-loglevel', 'warning', '-y', '-f', 'image2pipe',
    '-framerate', String(fps), '-vcodec', 'mjpeg', '-i', 'pipe:0',
    '-an', '-c:v', 'libx264', '-preset', 'fast', '-crf', '17',
    '-pix_fmt', 'yuv420p', '-movflags', '+faststart', resolve(output, 'neon-rush.mp4'),
  ], { stdio: ['pipe', 'ignore', 'inherit'] });
  const encoderDone = once(encoder, 'close');
  const schedule = jumpSchedule();
  const actions = [{ time: 2, action: 'Play' }, { time: 8, action: 'Pause' }, { time: 9, action: 'Resume' }];
  let jumpIndex = 0;
  for (let frame = 0; frame < duration * fps; frame++) {
    const time = frame / fps;
    if (frame === 120) {
      const box = await page.locator('#play-button').boundingBox();
      await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
      await page.waitForFunction(() => !document.querySelector('#game-hud').hidden);
    }
    if (frame === 480 || frame === 540) await page.keyboard.press('Escape');
    const playing = frame >= 120 && !(frame >= 480 && frame < 540);
    const elapsed = time - 2 - (frame >= 540 ? 1 : 0);
    if (playing && jumpIndex < schedule.length && elapsed >= schedule[jumpIndex]) {
      await page.keyboard.press('Space');
      actions.push({ time, action: 'Jump' });
      jumpIndex++;
    }
    await page.clock.runFor(1000 / fps);
    const shot = await page.screenshot({ type: 'jpeg', quality: 97 });
    if (!encoder.stdin.write(shot)) await once(encoder.stdin, 'drain');
    if (frame && frame % 180 === 0) console.log(`Captured ${time}s / ${duration}s`);
  }
  encoder.stdin.end();
  const [exitCode] = await encoderDone;
  if (exitCode !== 0) throw new Error(`FFmpeg exited ${exitCode}`);
  const endState = await page.evaluate(() => ({
    attempts: document.querySelector('#attempt').textContent,
    progress: document.querySelector('#progress-bar').getAttribute('aria-valuenow'),
    paused: !document.querySelector('#pause-screen').hidden,
    retry: !document.querySelector('#retry-notice').hidden,
  }));
  if (browserErrors.length) throw new Error(browserErrors.join('\n'));
  if (endState.attempts !== '01' || endState.paused || endState.retry) throw new Error(`Capture did not complete cleanly: ${JSON.stringify(endState)}`);
  await writeFile(resolve(output, 'capture.json'), JSON.stringify({
    duration, fps, width: 1920, height: 1080, audio: false,
    source: 'Unchanged retained Neon Rush game; real browser frames and keyboard inputs.',
    actions: actions.sort((a, b) => a.time - b.time), endState, browserErrors, edit,
  }, null, 2) + '\n');
  createEdit();
  console.log('Capture and six-second edit ready', endState);
} finally {
  encoder?.stdin.destroy();
  await browser.close();
}
