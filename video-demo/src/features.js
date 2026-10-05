(() => {
  "use strict";

  const WIDTH = 444, HEIGHT = 142, BEAT = 60 / 140;
  const CYAN = "#45edff", PINK = "#fb52d7";
  // Feature illustrations, driven only by the film clock; never run telemetry.
  const START = 14;
  const ARP_ORDER = [0, 2, 1, 3, 2, 1, 3, 2];
  const PERIOD = 4 * BEAT, JUMP_START = Math.ceil(5 / PERIOD) * PERIOD;
  const fade = (time, start) => Math.max(0, Math.min(1, (time - start) / 0.4));

  function surface(id, paint, time) {
    const canvas = document.getElementById(id);
    if (!canvas) return;
    // Draw at the output's pixel density so previews stay sharp in 4K.
    const density = Math.max(1, Math.round(window.devicePixelRatio || 1));
    if (canvas.width !== WIDTH * density) canvas.width = WIDTH * density;
    if (canvas.height !== HEIGHT * density) canvas.height = HEIGHT * density;
    const c = canvas.getContext("2d");
    if (!c) return;
    c.save();
    c.setTransform(canvas.width / WIDTH, 0, 0, canvas.height / HEIGHT, 0, 0);
    c.globalAlpha = 1;
    c.shadowBlur = 0;
    c.clearRect(0, 0, WIDTH, HEIGHT);
    const background = c.createLinearGradient(0, 0, 0, HEIGHT);
    background.addColorStop(0, "#0a1023");
    background.addColorStop(0.7, "#101b33");
    background.addColorStop(1, "#080e1c");
    c.fillStyle = background;
    c.fillRect(0, 0, WIDTH, HEIGHT);
    paint(c, time);
    c.restore();
  }

  function physics(c, time) {
    // Original speed, jump, and gravity at half scale; one leap every four beats.
    const jumpTime = Math.max(0, time - JUMP_START), phase = jumpTime % PERIOD;
    const speed = 140, jump = 350, gravity = 1050;
    const flight = 2 * jump / gravity, air = Math.min(phase, flight);
    const lift = Math.max(0, jump * air - gravity * air * air / 2);
    const floor = 115, anchor = 185, size = 20;
    const glow = c.createLinearGradient(0, floor - 40, 0, floor);
    glow.addColorStop(0, "#45edff00");
    glow.addColorStop(1, "#45edff0c");
    c.fillStyle = glow;
    c.fillRect(0, floor - 40, WIDTH, 40);
    c.fillStyle = "#0a1325";
    c.fillRect(0, floor, WIDTH, HEIGHT - floor);
    c.strokeStyle = "#45edff16";
    c.lineWidth = 1;
    c.beginPath();
    for (let x = -(time * speed) % 60; x < WIDTH + 30; x += 60) {
      c.moveTo(x, floor);
      c.lineTo(x - 24, HEIGHT);
    }
    c.moveTo(0, 134);
    c.lineTo(WIDTH, 134);
    c.stroke();
    c.fillStyle = "#45edff9c";
    c.fillRect(0, floor, WIDTH, 1.5);
    const spacing = PERIOD * speed;
    const firstSpike = anchor + speed * (flight / 2 - phase);
    c.globalAlpha = fade(time, 6.5);
    for (let i = -2; i <= 4; i++) {
      const x = firstSpike + i * spacing;
      c.beginPath();
      c.moveTo(x - 9, floor);
      c.lineTo(x, floor - 22);
      c.lineTo(x + 9, floor);
      c.closePath();
      c.fillStyle = "#382043";
      c.fill();
      c.strokeStyle = PINK;
      c.lineWidth = 1.5;
      c.shadowColor = PINK;
      c.shadowBlur = 6;
      c.stroke();
    }
    c.globalAlpha = 1;
    c.save();
    c.translate(anchor, floor - size / 2 - lift);
    c.rotate((Math.floor(jumpTime / PERIOD) + air / flight) * Math.PI * 1.5);
    c.shadowColor = CYAN;
    c.shadowBlur = 9;
    c.fillStyle = CYAN;
    c.fillRect(-10, -10, size, size);
    c.shadowBlur = 0;
    c.strokeStyle = "#d9ffff";
    c.lineWidth = 1;
    c.strokeRect(-9, -9, 18, 18);
    c.fillStyle = "#103649";
    c.fillRect(-6, -6, 12, 12);
    c.fillStyle = CYAN;
    c.fillRect(-4, -3, 2, 3);
    c.fillRect(2, -3, 2, 3);
    c.fillRect(-4, 3, 8, 1.5);
    c.restore();
  }

  function sequencer(c, time) {
    // Four lanes assemble the score: kick, bass, then low/high arpeggio voices.
    const left = 24, top = 17, column = (WIDTH - left * 2) / 8, row = 27;
    const step = (time / (BEAT / 2)) % 8;
    const active = Math.floor(step);
    c.fillStyle = "#45edff0c";
    c.fillRect(left + active * column, top, column, row * 4);
    for (let pitch = 0; pitch < 4; pitch++) {
      const y = top + pitch * row;
      c.strokeStyle = "#243148";
      c.lineWidth = 1;
      c.strokeRect(left, y, column * 8, row);
    }
    for (let i = 0; i <= 8; i++) {
      c.beginPath();
      c.moveTo(left + i * column, top);
      c.lineTo(left + i * column, top + row * 4);
      c.strokeStyle = i % 2 ? "#1c2940" : "#2b3b53";
      c.stroke();
    }
    function note(at, lane, length, color, opacity) {
      const playing = step >= at && step < at + length;
      c.fillStyle = color;
      c.globalAlpha = opacity * (playing ? 1 : 0.53);
      c.shadowColor = color;
      c.shadowBlur = playing ? 8 : 0;
      c.fillRect(left + at * column + 8, top + lane * row + 8, column * length - 16, 11);
      c.shadowBlur = 0;
      c.globalAlpha = 1;
    }
    for (const beat of [0, 2, 4, 6]) note(beat, 3, 0.45, CYAN, 1);
    note(0, 2, 2.9, CYAN, fade(time, 5));
    note(4, 2, 2.6, CYAN, fade(time, 5));
    ARP_ORDER.forEach((pitch, i) =>
      note(i, pitch < 2 ? 1 : 0, 0.96, PINK, fade(time, 6.5)));
    c.fillStyle = "#d9ffff";
    c.fillRect(left + step * column, top - 3, 1.5, row * 4 + 6);
  }

  window.DELM_FEATURES = Object.freeze({
    draw(time) {
      const seconds = Math.max(0, Number.isFinite(time) ? time - START : 0);
      surface("physics-preview", physics, seconds);
      surface("audio-preview", sequencer, seconds);
    },
  });
})();
