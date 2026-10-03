(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.NeonCore = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';
  const BPM = 140;
  const BEAT = 60 / BPM;
  const SPEED = 280;
  const UNIT = SPEED * BEAT;
  const SIZE = 32;
  const GRAVITY = 2100;
  const JUMP = 700;
  const STEP = 1 / 120;
  const sections = [
    { beat: 0, name: 'First light', label: '01', color: '#45edff' },
    { beat: 32, name: 'Night drive', label: '02', color: '#45edff' },
    { beat: 64, name: 'Overdrive', label: '03', color: '#fb52d7' },
    { beat: 96, name: 'Event horizon', label: '04', color: '#fb52d7' },
    { beat: 124, name: 'Final ascent', label: '05', color: '#b6fff0' }
  ];
  const spikes = [], gaps = [], platforms = [];
  function spike(beat, count = 1, base = 0) {
    for (let i = 0; i < count; i++) spikes.push({ x: beat * UNIT + i * 30, w: 30, h: 35, base });
  }
  function gap(beat, width = 110) { gaps.push({ x: beat * UNIT, w: width }); }
  function platform(beat, width = 150, height = 48) { platforms.push({ x: beat * UNIT, w: width, h: height }); }
  // A composed course, in beat coordinates. Each phrase leaves a recovery runway.
  spike(6); spike(10); spike(14, 2); gap(18, 100); platform(22, 148, 44); spike(27, 2);
  spike(34, 2); gap(38, 116); platform(42, 150, 50); spike(46, 2);
  gap(50, 125); platform(54, 160, 54); spike(59, 3);
  spike(66, 3); platform(70, 148, 58); gap(74, 130); spike(78, 2);
  platform(82, 156, 56); spike(86, 3); gap(90, 136); spike(94, 2);
  platform(98, 144, 62); spike(102, 3); gap(106, 142);
  platform(110, 138, 64); spike(114, 3); gap(118, 145); spike(122, 2);
  spike(126, 3); platform(130, 144, 66); gap(134, 146);
  const level = { bpm: BPM, beat: BEAT, beats: 140, duration: 60, speed: SPEED,
    length: SPEED * 60, sections, spikes, gaps, platforms };

  function overlap(a, b) { return a.left < b.right && a.right > b.left && a.top < b.bottom && a.bottom > b.top; }
  function spikeHit(rect, s) {
    const triangle = [{ x: s.x + 2, y: -s.base }, { x: s.x + s.w / 2, y: -s.base - s.h + 2 }, { x: s.x + s.w - 2, y: -s.base }];
    const box = [{ x: rect.left, y: rect.top }, { x: rect.right, y: rect.top }, { x: rect.right, y: rect.bottom }, { x: rect.left, y: rect.bottom }];
    const axes = [{ x: 1, y: 0 }, { x: 0, y: 1 }];
    for (let i = 0; i < 3; i++) {
      const p = triangle[i], q = triangle[(i + 1) % 3];
      axes.push({ x: -(q.y - p.y), y: q.x - p.x });
    }
    return axes.every(axis => {
      const a = triangle.map(p => p.x * axis.x + p.y * axis.y);
      const b = box.map(p => p.x * axis.x + p.y * axis.y);
      return Math.max(...a) > Math.min(...b) && Math.max(...b) > Math.min(...a);
    });
  }
  class Simulation {
    constructor(course = level) { this.level = course; this.reset(); }
    reset() {
      this.x = 0; this.y = -SIZE; this.vy = 0; this.elapsed = 0;
      this.grounded = true; this.coyote = 0.055; this.rotation = 0;
      this.status = 'running'; this.accumulator = 0; this.steps = 0; this.events = [];
      this.inputBuffer = 0; this.wasHeld = false;
    }
    get progress() { return Math.min(1, this.x / this.level.length); }
    get section() {
      const beat = this.elapsed / BEAT;
      return this.level.sections.filter(s => beat >= s.beat).at(-1) || this.level.sections[0];
    }
    die(reason) {
      if (this.status !== 'running') return;
      this.status = 'dead'; this.events.push({ type: 'death', reason });
    }
    advance(seconds, held = false) {
      this.events = [];
      if (this.status !== 'running') return this.events;
      if (held && !this.wasHeld) this.inputBuffer = 0.09;
      this.wasHeld = held;
      // The browser runner pauses on hidden tabs. A stall cannot teleport through hazards.
      this.accumulator += Math.min(0.1, Math.max(0, seconds));
      while (this.accumulator + 1e-9 >= STEP && this.status === 'running') {
        this.tick(held); this.accumulator -= STEP;
      }
      return this.events;
    }
    tick(held) {
      const oldBottom = this.y + SIZE;
      if ((held || this.inputBuffer > 0) && (this.grounded || this.coyote > 0)) {
        this.vy = -JUMP; this.grounded = false; this.coyote = 0;
        this.inputBuffer = 0;
        this.events.push({ type: 'jump' });
      }
      this.inputBuffer = Math.max(0, this.inputBuffer - STEP);
      this.steps++;
      this.elapsed = this.steps * STEP;
      this.x = this.elapsed * this.level.speed;
      // Exact constant-acceleration integration, independent of display refresh rate.
      this.y += this.vy * STEP + 0.5 * GRAVITY * STEP * STEP;
      this.vy += GRAVITY * STEP;
      if (!this.grounded) this.rotation += Math.PI * 1.5 * STEP / (2 * JUMP / GRAVITY);
      let support = false;
      const left = this.x + 4, right = this.x + SIZE - 4;
      if (this.vy >= 0) {
        for (const p of this.level.platforms) {
          if (right > p.x && left < p.x + p.w && oldBottom <= -p.h + 0.1 && this.y + SIZE >= -p.h) {
            this.y = -p.h - SIZE; this.vy = 0; support = true; break;
          }
        }
        const inGap = this.level.gaps.some(g => left >= g.x && right <= g.x + g.w);
        if (!support && !inGap && oldBottom <= 0.1 && this.y + SIZE >= 0) {
          this.y = -SIZE; this.vy = 0; support = true;
        }
      }
      if (support) {
        if (!this.grounded) this.events.push({ type: 'land' });
        this.grounded = true; this.coyote = 0.055;
        this.rotation = Math.round(this.rotation / (Math.PI / 2)) * Math.PI / 2;
      } else { this.grounded = false; this.coyote = Math.max(0, this.coyote - STEP); }
      const rect = { left, right, top: this.y + 4, bottom: this.y + SIZE - 4 };
      for (const p of this.level.platforms) {
        if (overlap(rect, { left: p.x, right: p.x + p.w, top: -p.h + 2, bottom: 20 })) this.die('platform');
      }
      for (const s of this.level.spikes) {
        if (s.x > right || s.x + s.w < left) continue;
        if (spikeHit(rect, s)) this.die('spike');
      }
      if (this.y > 150) this.die('gap');
      if (this.status === 'running' && this.x >= this.level.length) {
        this.x = this.level.length; this.status = 'complete'; this.events.push({ type: 'finish' });
      }
    }
  }
  return { Simulation, level, constants: { BPM, BEAT, SPEED, UNIT, SIZE, GRAVITY, JUMP, STEP }, spikeHit, overlap };
});
