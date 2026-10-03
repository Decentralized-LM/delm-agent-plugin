(function () {
  'use strict';
  const { level, constants } = NeonCore;
  const CYAN = '#45edff', PINK = '#fb52d7';
  const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  const hash = n => { const x = Math.sin(n * 127.1 + 311.7) * 43758.5453; return x - Math.floor(x); };
  class NeonRenderer {
    constructor(canvas) {
      this.canvas = canvas; this.ctx = canvas.getContext('2d', { alpha: false });
      this.width = 1; this.height = 1; this.scale = 1; this.particles = []; this.trail = [];
      this.shake = 0; this.flash = 0; this.trailClock = 0; this.resize();
    }
    resize() {
      const box = this.canvas.getBoundingClientRect();
      this.width = box.width; this.height = box.height;
      const ratio = Math.min(2, window.devicePixelRatio || 1);
      this.canvas.width = Math.round(box.width * ratio); this.canvas.height = Math.round(box.height * ratio);
      this.ratio = ratio;
      this.scale = Math.min(1.6, this.width / 820, this.height / 430);
      // On portrait screens preserve 660 world units of visible runway.
      if (this.width < 600 && this.height > this.width) this.scale = this.width / 660;
      this.ground = this.height * (this.height < 380 ? 0.77 : 0.72);
      this.anchor = Math.max(68, this.width * 0.205);
    }
    clear() { this.particles = []; this.trail = []; this.shake = 0; this.flash = 0; this.trailClock = 0; }
    emit(x, y, count, color, strength = 1) {
      for (let i = 0; i < count; i++) {
        const angle = Math.random() * Math.PI * 2;
        const speed = (25 + Math.random() * 145) * strength;
        this.particles.push({ x, y, vx: Math.cos(angle) * speed, vy: Math.sin(angle) * speed - 50,
          age: 0, life: 0.25 + Math.random() * 0.5, size: 2 + Math.random() * 5, color });
      }
    }
    event(type, sim) {
      if (type === 'jump') this.emit(sim.x + 16, sim.y + 32, 9, CYAN, 0.6);
      if (type === 'land') this.emit(sim.x + 16, sim.y + 32, 5, '#95f4ff', 0.4);
      if (type === 'death') { this.emit(sim.x + 16, sim.y + 16, 38, PINK, 1.8); this.emit(sim.x + 16, sim.y + 16, 18, CYAN, 1.2); this.shake = reducedMotion ? 0 : 6; this.flash = reducedMotion ? 0 : 0.13; }
      if (type === 'finish') { this.emit(sim.x + 16, -80, 75, CYAN, 3); this.emit(sim.x, -90, 45, PINK, 3); }
    }
    update(dt, sim, playing) {
      this.shake = Math.max(0, this.shake - dt * 32); this.flash = Math.max(0, this.flash - dt);
      for (const p of this.particles) { p.age += dt; p.x += p.vx * dt; p.y += p.vy * dt; p.vy += 260 * dt; }
      this.particles = this.particles.filter(p => p.age < p.life);
      for (const t of this.trail) t.age += dt;
      this.trail = this.trail.filter(t => t.age < 0.28);
      this.trailClock += dt;
      if (playing && sim.status === 'running' && this.trailClock > 0.016) {
        this.trail.push({ x: sim.x, y: sim.y, rotation: sim.rotation, age: 0 }); this.trailClock = 0;
      }
    }
    render(sim, state, time) {
      const c = this.ctx, w = this.width, h = this.height;
      c.setTransform(this.ratio, 0, 0, this.ratio, 0, 0);
      c.globalAlpha = 1; c.shadowBlur = 0;
      const menu = state === 'title';
      const seconds = menu ? time * 0.3 : sim.elapsed;
      const scroll = menu ? time * 24 : sim.x;
      const beat = seconds / constants.BEAT;
      const pulse = reducedMotion ? 0.15 : Math.pow(1 - beat % 1, 4);
      const pinkPhase = !menu && sim.elapsed >= constants.BEAT * 64;
      const accent = pinkPhase ? PINK : CYAN;
      this.background(scroll, seconds, pulse, menu, pinkPhase);
      c.save();
      if (this.shake) c.translate((Math.random() - 0.5) * this.shake, (Math.random() - 0.5) * this.shake);
      if (menu) this.hero(time, pulse);
      else {
        c.translate(this.anchor - sim.x * this.scale, this.ground); c.scale(this.scale, this.scale);
        this.world(sim, pulse, accent);
        for (const t of this.trail) {
          c.save(); c.globalAlpha = (1 - t.age / 0.28) * 0.2; c.translate(t.x + 16, t.y + 16); c.rotate(t.rotation);
          c.fillStyle = CYAN; c.fillRect(-14, -14, 28, 28); c.restore();
        }
        if (sim.status !== 'dead') this.cube(sim.x + 16, sim.y + 16, 32, sim.rotation, pulse);
        this.drawParticles();
      }
      c.restore();
      if (this.flash) { c.fillStyle = `rgba(251,82,215,${this.flash * 0.65})`; c.fillRect(0, 0, w, h); }
    }
    background(scroll, time, pulse, menu, pinkPhase) {
      const c = this.ctx, w = this.width, h = this.height, g = this.ground;
      const gradient = c.createLinearGradient(0, 0, 0, h);
      gradient.addColorStop(0, '#0a1023'); gradient.addColorStop(0.6, pinkPhase ? '#18132e' : '#101b33'); gradient.addColorStop(1, '#080e1c');
      c.fillStyle = gradient; c.fillRect(0, 0, w, h);
      const glowX = menu ? w * 0.76 : w * 0.72;
      const glowY = menu ? h * 0.44 : g - h * 0.22;
      const glow = c.createRadialGradient(glowX, glowY, 0, glowX, glowY, h * 0.6);
      glow.addColorStop(0, `rgba(145,40,153,${0.11 + pulse * 0.035})`); glow.addColorStop(1, 'rgba(65,25,100,0)');
      c.fillStyle = glow; c.fillRect(0, 0, w, h);
      // Fine, slow parallax grid and a deterministic star field.
      c.lineWidth = 1; c.strokeStyle = '#50648b0b'; c.beginPath();
      const grid = 64;
      for (let x = -scroll * 0.08 % grid; x < w; x += grid) { c.moveTo(x, 0); c.lineTo(x, h); }
      for (let y = 15; y < h; y += grid) { c.moveTo(0, y); c.lineTo(w, y); } c.stroke();
      for (let i = 0; i < 58; i++) {
        const x = ((hash(i) * w * 2 - scroll * 0.025) % w + w) % w;
        const y = hash(i + 99) * g * 0.88;
        c.fillStyle = `rgba(161,189,233,${0.14 + hash(i + 300) * 0.22})`; c.fillRect(x, y, i % 9 === 0 ? 2 : 1, 1);
      }
      // A cut-line sun: geometric, quiet enough to keep hazards readable.
      const radius = Math.min(w * (menu ? 0.15 : 0.105), h * 0.255);
      c.save(); c.beginPath(); c.arc(glowX, glowY, radius, 0, Math.PI * 2); c.clip();
      const sun = c.createLinearGradient(0, glowY - radius, 0, glowY + radius);
      sun.addColorStop(0, '#fb52d720'); sun.addColorStop(1, '#fb52d706'); c.fillStyle = sun;
      c.fillRect(glowX - radius, glowY - radius, radius * 2, radius * 2);
      c.strokeStyle = '#fb52d726'; c.lineWidth = 1;
      for (let y = glowY - radius; y < glowY + radius; y += 12) { c.beginPath(); c.moveTo(glowX - radius, y); c.lineTo(glowX + radius, y); c.stroke(); }
      c.restore();
      c.beginPath(); c.arc(glowX, glowY, radius + 8, Math.PI * 1.1, Math.PI * 1.8); c.strokeStyle = '#fb52d723'; c.stroke();
      this.mountains(scroll * 0.12, g - h * 0.10, h * 0.20, 160, '#111b33', '#35426833', 12);
      this.city(scroll * 0.23, g, h);
      this.mountains(scroll * 0.37, g + 4, h * 0.10, 140, '#0c172b', '#27546b48', 57);
      const horizon = c.createLinearGradient(0, g - 35, 0, g + 5);
      horizon.addColorStop(0, '#45edff00'); horizon.addColorStop(1, `rgba(69,237,255,${0.028 + pulse * 0.018})`);
      c.fillStyle = horizon; c.fillRect(0, g - 35, w, 40);
    }
    mountains(scroll, baseline, amplitude, spacing, fill, stroke, seed) {
      const c = this.ctx; const start = Math.floor(scroll / spacing) - 1;
      c.beginPath(); c.moveTo(-spacing, this.height);
      for (let i = start; i < start + Math.ceil(this.width / spacing) + 4; i++) c.lineTo(i * spacing - scroll, baseline - hash(i + seed) * amplitude);
      c.lineTo(this.width + spacing, this.height); c.closePath(); c.fillStyle = fill; c.fill(); c.strokeStyle = stroke; c.lineWidth = 1; c.stroke();
    }
    city(scroll, baseline, h) {
      const c = this.ctx, step = 67, start = Math.floor(scroll / step) - 1;
      for (let i = start; i < start + Math.ceil(this.width / step) + 3; i++) {
        const x = i * step - scroll, height = 24 + hash(i + 34) * h * 0.15;
        const width = 28 + hash(i + 63) * 27;
        c.fillStyle = '#0b1428'; c.fillRect(x, baseline - height, width, height);
        c.strokeStyle = '#29436444'; c.lineWidth = 1; c.strokeRect(x, baseline - height, width, height);
        c.fillStyle = '#45edff13';
        for (let yy = baseline - height + 8; yy < baseline - 10; yy += 11) {
          for (let xx = x + 6; xx < x + width - 4; xx += 9) if (hash(xx + yy) > 0.42) c.fillRect(xx, yy, 2, 3);
        }
      }
    }
    floor(start, end, accent, pulse) {
      if (end <= start) return;
      const c = this.ctx, depth = this.height / this.scale;
      c.fillStyle = '#0a1325'; c.fillRect(start, 0, end - start, depth);
      c.save(); c.beginPath(); c.rect(start, 0, end - start, depth); c.clip();
      c.strokeStyle = '#45edff0d'; c.lineWidth = 1; c.beginPath();
      for (let x = Math.floor(start / 60) * 60; x < end; x += 60) { c.moveTo(x, 0); c.lineTo(x - 45, depth); }
      for (let y = 25; y < depth; y += 30) { c.moveTo(start, y); c.lineTo(end, y); } c.stroke();
      c.restore();
      c.fillStyle = accent; c.globalAlpha = 0.65 + pulse * 0.2; c.fillRect(start, -1, end - start, 2); c.globalAlpha = 1;
      c.fillStyle = '#45edff0c'; c.fillRect(start, 2, end - start, 8);
    }
    world(sim, pulse, accent) {
      const c = this.ctx, left = sim.x - this.anchor / this.scale - 60, right = left + this.width / this.scale + 120;
      let floorStart = left;
      for (const g of level.gaps) {
        if (g.x + g.w < left || g.x > right) continue;
        this.floor(floorStart, g.x, accent, pulse); floorStart = g.x + g.w;
        const abyss = c.createLinearGradient(0, 0, 0, 160); abyss.addColorStop(0, '#fb52d71a'); abyss.addColorStop(1, '#fb52d700');
        c.fillStyle = abyss; c.fillRect(g.x, 0, g.w, 160);
        c.strokeStyle = '#fb52d7a0'; c.lineWidth = 2; c.beginPath(); c.moveTo(g.x, 0); c.lineTo(g.x, 23); c.moveTo(g.x + g.w, 0); c.lineTo(g.x + g.w, 23); c.stroke();
        // Small edge beacons communicate the gap before it reaches the cube.
        c.fillStyle = PINK; c.fillRect(g.x - 6, -3, 6, 3); c.fillRect(g.x + g.w, -3, 6, 3);
      }
      this.floor(floorStart, right, accent, pulse);
      for (const s of level.spikes) if (s.x + s.w >= left && s.x <= right) this.spike(s, pulse);
      for (const p of level.platforms) if (p.x + p.w >= left && p.x <= right) this.platform(p, pulse);
      for (const section of level.sections.slice(1)) {
        const x = section.beat * constants.UNIT;
        if (x < left || x > right) continue;
        c.save(); c.globalAlpha = 0.27; c.strokeStyle = section.color; c.lineWidth = 1;
        c.setLineDash([3, 9]); c.beginPath(); c.moveTo(x, -240); c.lineTo(x, 0); c.stroke(); c.setLineDash([]);
        c.font = '8px monospace'; c.fillStyle = section.color; c.fillText(`${section.label} / ${section.name.toUpperCase()}`, x + 10, -220); c.restore();
      }
      const finish = level.length + 16;
      if (finish < right + 100) {
        c.save(); c.strokeStyle = CYAN; c.shadowColor = CYAN; c.shadowBlur = 15; c.lineWidth = 3;
        c.strokeRect(finish, -170, 65, 170); c.strokeStyle = '#fb52d790'; c.strokeRect(finish + 10, -158, 45, 158);
        c.fillStyle = '#45edff14'; c.fillRect(finish, -170, 65, 170); c.shadowBlur = 0;
        c.font = '9px monospace'; c.fillStyle = CYAN; c.fillText('FINISH', finish + 5, -185); c.restore();
      }
    }
    spike(s, pulse) {
      const c = this.ctx; c.save();
      c.beginPath(); c.moveTo(s.x, -s.base); c.lineTo(s.x + s.w / 2, -s.base - s.h); c.lineTo(s.x + s.w, -s.base); c.closePath();
      c.fillStyle = '#382043'; c.fill(); c.strokeStyle = PINK; c.lineWidth = 1.7;
      c.shadowColor = PINK; c.shadowBlur = 7 + pulse * 5; c.stroke(); c.shadowBlur = 0;
      c.beginPath(); c.moveTo(s.x + 9, -s.base - 5); c.lineTo(s.x + s.w / 2, -s.base - s.h + 15); c.lineTo(s.x + s.w - 9, -s.base - 5); c.strokeStyle = '#fb52d766'; c.lineWidth = 1; c.stroke();
      c.restore();
    }
    platform(p, pulse) {
      const c = this.ctx; c.save(); c.fillStyle = '#13273b'; c.fillRect(p.x, -p.h, p.w, p.h);
      c.strokeStyle = '#45edff70'; c.lineWidth = 1; c.strokeRect(p.x, -p.h, p.w, p.h);
      c.strokeStyle = CYAN; c.shadowColor = CYAN; c.shadowBlur = 6 + pulse * 4; c.lineWidth = 2; c.beginPath(); c.moveTo(p.x, -p.h); c.lineTo(p.x + p.w, -p.h); c.stroke(); c.shadowBlur = 0;
      c.strokeStyle = '#45edff16';
      for (let x = p.x + 12; x < p.x + p.w - 8; x += 17) { c.beginPath(); c.moveTo(x, -p.h + 10); c.lineTo(x - 5, -8); c.stroke(); }
      c.fillStyle = '#b7f8ff'; c.fillRect(p.x + 5, -p.h + 5, 4, 4); c.fillRect(p.x + p.w - 9, -p.h + 5, 4, 4); c.restore();
    }
    cube(x, y, size, rotation, pulse) {
      const c = this.ctx, half = size / 2; c.save(); c.translate(x, y); c.rotate(rotation);
      c.shadowColor = CYAN; c.shadowBlur = 13 + pulse * 5; c.fillStyle = CYAN; c.fillRect(-half, -half, size, size); c.shadowBlur = 0;
      c.strokeStyle = '#d9ffff'; c.lineWidth = 1.3; c.strokeRect(-half + 1, -half + 1, size - 2, size - 2);
      c.fillStyle = '#103649'; c.fillRect(-half + size * 0.17, -half + size * 0.17, size * 0.66, size * 0.66);
      c.fillStyle = CYAN; c.fillRect(-size * 0.20, -size * 0.17, size * 0.11, size * 0.19); c.fillRect(size * 0.09, -size * 0.17, size * 0.11, size * 0.19);
      c.fillRect(-size * 0.20, size * 0.13, size * 0.40, size * 0.07); c.restore();
    }
    drawParticles() {
      const c = this.ctx;
      for (const p of this.particles) { c.globalAlpha = 1 - p.age / p.life; c.fillStyle = p.color; c.fillRect(p.x, p.y, p.size, p.size); } c.globalAlpha = 1;
    }
    hero(time, pulse) {
      const c = this.ctx, w = this.width, h = this.height;
      const small = w < 600;
      const x = w * (small ? 0.80 : 0.745), y = this.ground - (small ? 78 : 105);
      const size = small ? 38 : Math.min(79, w * 0.065);
      const ring = small ? 95 : Math.min(w * 0.20, h * 0.39);
      c.save(); c.translate(x, y); c.rotate(-Math.PI / 4);
      for (let i = 0; i < 3; i++) {
        const r = ring + i * 20; c.strokeStyle = i === 0 ? '#45edff1b' : '#45edff0a'; c.lineWidth = 1;
        c.strokeRect(-r / 2, -r / 2, r, r);
      }
      c.restore();
      const drift = reducedMotion ? 0 : Math.sin(time * 1.5) * 8;
      for (let i = 5; i > 0; i--) {
        c.save(); c.globalAlpha = (1 - i / 6) * 0.12; c.translate(x - i * 22, y + i * 14 + drift); c.rotate(-Math.PI / 5); c.fillStyle = CYAN; c.fillRect(-size / 2, -size / 2, size, size); c.restore();
      }
      this.cube(x, y + drift, size, -Math.PI / 5 + (reducedMotion ? 0 : Math.sin(time) * 0.05), pulse);
      c.save(); c.translate(0, this.ground); const scale = small ? 0.8 : 1.3; c.scale(scale, scale);
      this.floor(0, w / scale, CYAN, pulse);
      this.spike({ x: w * 0.72 / scale, w: 30, h: 35, base: 0 }, pulse);
      this.spike({ x: w * 0.72 / scale + 30, w: 30, h: 35, base: 0 }, pulse);
      this.platform({ x: w * 0.91 / scale, w: w * 0.15 / scale, h: 46 }, pulse); c.restore();
      if (!small) {
        c.font = '9px monospace'; c.fillStyle = '#728da9'; c.fillText('LESS HESITATION. MORE MOMENTUM.', x - 100, this.ground + 40);
        c.fillStyle = '#45edff60'; c.fillText('↗  NR — 001', x + 65, y - 65);
      }
    }
  }
  window.NeonRenderer = NeonRenderer;
})();
