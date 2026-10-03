const test = require('node:test');
const assert = require('node:assert/strict');
const { Simulation, level, constants, spikeHit } = require('../js/core.js');
const blank = () => ({ ...level, length: 1e6, spikes: [], gaps: [], platforms: [] });
function run(sim, seconds, fps = 60, held = false) {
  for (let i = 0; i < Math.round(seconds * fps); i++) sim.advance(1 / fps, held);
  return sim;
}
test('automatic movement and jump arc agree at 30, 60, and 144 Hz', () => {
  const runs = [30, 60, 144].map(fps => run(new Simulation(blank()), 2, fps, true));
  for (const sim of runs) {
    assert.equal(sim.x, 560); assert.equal(sim.steps, 240);
    assert.ok(Math.abs(sim.y - runs[0].y) < 1e-8);
    assert.ok(Math.abs(sim.vy - runs[0].vy) < 1e-8);
  }
});
test('holding input repeats jumps on landing and releasing stays grounded', () => {
  const sim = new Simulation(blank()); let jumps = 0;
  for (let i = 0; i < 240; i++) jumps += sim.advance(1 / 120, true).filter(e => e.type === 'jump').length;
  assert.equal(jumps, 3); run(sim, 1); assert.equal(sim.y, -32); assert.equal(sim.grounded, true);
});
test('triangle collisions forgive empty bounding-box corners', () => {
  const spike = { x: 100, w: 30, h: 35, base: 0 };
  assert.equal(spikeHit({ left: 99, right: 106, top: -35, bottom: -26 }, spike), false);
  assert.equal(spikeHit({ left: 112, right: 125, top: -28, bottom: -6 }, spike), true);
});
test('spikes, gaps and platform sides kill; platform tops support the cube', () => {
  const spike = new Simulation({ ...blank(), spikes: [{ x: 100, w: 30, h: 35, base: 0 }] });
  run(spike, 1); assert.equal(spike.status, 'dead');
  const pit = new Simulation({ ...blank(), gaps: [{ x: 60, w: 240 }] });
  run(pit, 2); assert.equal(pit.status, 'dead');
  const side = new Simulation({ ...blank(), platforms: [{ x: 80, w: 150, h: 50 }] });
  run(side, 1); assert.equal(side.status, 'dead');
  const top = new Simulation({ ...blank(), platforms: [{ x: 80, w: 180, h: 50 }] });
  top.advance(1 / 120, true); run(top, 0.61);
  assert.equal(top.status, 'running'); assert.equal(top.y, -82); assert.equal(top.grounded, true);
});
test('reset removes death, progress, velocity and stale input events', () => {
  const sim = new Simulation(); run(sim, 4); assert.equal(sim.status, 'dead'); sim.reset();
  assert.equal(sim.status, 'running'); assert.equal(sim.x, 0); assert.equal(sim.y, -32);
  assert.equal(sim.vy, 0); assert.equal(sim.elapsed, 0); assert.equal(sim.progress, 0);
  assert.deepEqual(sim.events, []); assert.equal(sim.accumulator, 0);
});
test('completion fires once at the finish, and freezes further movement', () => {
  const sim = new Simulation({ ...blank(), length: 280 }); let finishes = 0;
  for (let i = 0; i < 180; i++) finishes += sim.advance(1 / 60).filter(e => e.type === 'finish').length;
  assert.equal(sim.status, 'complete'); assert.equal(sim.progress, 1); assert.equal(sim.x, 280); assert.equal(finishes, 1);
});

// Solve each authored encounter with the actual 120 Hz collision/physics model.
// Save a single continuous input schedule, then replay all 60 seconds at display rates.
const encounters = [
  ...level.spikes.filter((s, i, a) => !a.some((p, j) => j < i && s.x - p.x === 30)).map(s => ({ x: s.x, kind: 'spike' })),
  ...level.gaps.map(g => ({ x: g.x, kind: 'gap' })),
  ...level.platforms.map(p => ({ x: p.x, kind: 'platform' }))
].sort((a, b) => a.x - b.x);
function snapshot(sim) { return Object.assign(Object.create(Object.getPrototypeOf(sim)), sim, { events: [...sim.events] }); }
function solve() {
  let sim = new Simulation(); const jumps = [];
  for (const hazard of encounters) {
    const target = Math.min(hazard.x + 290, level.length);
    let candidate = null;
    // Choose the middle of the viable timing window, not a frame-perfect solution.
    const valid = [];
    for (let distance = -12; distance <= 155; distance += 2) {
      const clone = snapshot(sim); let jumpAt = null;
      while (clone.x < target && clone.status === 'running') {
        const held = jumpAt === null && clone.x + constants.SIZE >= hazard.x - distance;
        if (held) jumpAt = clone.steps;
        clone.advance(constants.STEP, held);
      }
      if (clone.status !== 'dead' && jumpAt !== null) valid.push({ clone, jumpAt, distance });
    }
    assert.ok(valid.length >= 8, `${hazard.kind} at beat ${hazard.x / constants.UNIT}: needs a fair jump window, found ${valid.length}`);
    candidate = valid[Math.floor(valid.length / 2)]; jumps.push(candidate.jumpAt); sim = candidate.clone;
  }
  while (sim.status === 'running') sim.advance(constants.STEP);
  assert.equal(sim.status, 'complete'); return jumps;
}
test('the complete hand-authored course has generous jump windows and a winning replay', () => {
  const jumps = solve();
  for (const fps of [30, 60, 144]) {
    const sim = new Simulation(); let j = 0;
    for (let frame = 0; frame < fps * 61 && sim.status === 'running'; frame++) {
      const due = j < jumps.length && sim.steps >= jumps[j] - 1;
      if (due) j++;
      sim.advance(1 / fps, due);
    }
    assert.equal(sim.status, 'complete', `winning replay at ${fps} Hz (reached ${sim.progress * 100}%)`);
    assert.equal(sim.elapsed, 60); assert.equal(sim.progress, 1);
  }
});
