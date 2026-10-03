const test = require('node:test');
const assert = require('node:assert/strict');
const { NeonAudio, SCORE, BPM, BEAT_SECONDS, DURATION_SECONDS, SECTION_BEATS } = require('../js/audio.js');
const { level } = require('../js/core.js');

class Param {
  constructor() { this.value = 0; this.events = []; }
  record(method, value, time) {
    assert.ok(Number.isFinite(value), `${method}: finite value`);
    assert.ok(Number.isFinite(time) && time >= 0, `${method}: valid audio time`);
    this.value = value; this.events.push({ method, value, time }); return this;
  }
  setValueAtTime(value, time) { return this.record('set', value, time); }
  linearRampToValueAtTime(value, time) { return this.record('linear', value, time); }
  exponentialRampToValueAtTime(value, time) {
    assert.ok(value > 0, 'exponential ramps must remain strictly positive');
    return this.record('exponential', value, time);
  }
  setTargetAtTime(value, time) { return this.record('target', value, time); }
}
class FakeNode {
  constructor(context, kind) {
    this.context = context; this.kind = kind; this.connections = []; this.disconnected = false;
    ['gain', 'frequency', 'detune', 'Q', 'pan', 'delayTime', 'threshold', 'knee', 'ratio', 'attack', 'release']
      .forEach(name => { this[name] = new Param(); });
  }
  connect(target) { this.connections.push(target); return target; }
  disconnect() { this.disconnected = true; this.connections = []; }
  start(time, offset) {
    assert.ok(time >= this.context.currentTime, 'no new source starts in the past');
    this.startedAt = time; this.context.starts.push({ node: this, time, offset });
  }
  stop(time) { this.stoppedAt = time; }
}
class FakeContext {
  constructor() {
    this.currentTime = 0; this.sampleRate = 8000; this.state = 'suspended';
    this.destination = {}; this.starts = []; this.nodes = [];
  }
  async resume() { this.state = 'running'; }
  make(kind) { const node = new FakeNode(this, kind); this.nodes.push(node); return node; }
  createDynamicsCompressor() { return this.make('compressor'); }
  createGain() { return this.make('gain'); }
  createBiquadFilter() { return this.make('filter'); }
  createDelay() { return this.make('delay'); }
  createStereoPanner() { return this.make('pan'); }
  createOscillator() { return this.make('oscillator'); }
  createBufferSource() { return this.make('noise'); }
  createBuffer(channels, length) { return { getChannelData: () => new Float32Array(length) }; }
}
async function setup() {
  const ctx = new FakeContext();
  const audio = new NeonAudio({ contextFactory: () => ctx });
  assert.equal(await audio.unlock(), true);
  return { audio, ctx };
}

test('score is exactly 60 seconds, aligned to the level, and contains a composed arrangement', () => {
  assert.equal(BPM, 140); assert.equal(DURATION_SECONDS, 60);
  assert.equal(level.duration, DURATION_SECONDS); assert.equal(level.bpm, BPM);
  assert.deepEqual(level.sections.map(section => section.beat), SECTION_BEATS);
  assert.ok(SCORE.length > 1000);
  for (let index = 0; index < SCORE.length; index++) {
    const event = SCORE[index];
    assert.ok(event.beat >= 0 && event.beat + event.duration <= 140);
    assert.ok(event.duration > 0);
    if (index) assert.ok(event.beat >= SCORE[index - 1].beat);
  }
  assert.equal(SCORE.filter(event => event.type === 'kick').length, 140);
  ['clap', 'hat', 'bass', 'arp', 'lead', 'pad', 'counter', 'riser'].forEach(type =>
    assert.ok(SCORE.some(event => event.type === type), `instrument ${type}`));
  assert.ok(new Set(SCORE.filter(event => event.type === 'lead').map(event => event.note)).size >= 10);
  const density = (begin, end) => SCORE.filter(event => event.beat >= begin && event.beat < end).length / (end - begin);
  assert.ok(density(96, 124) > density(32, 64));
  assert.ok(density(124, 140) > density(0, 32));
});

test('audio is only created by unlock, and absence or denial of Web Audio is harmless', async () => {
  let created = 0;
  const absent = new NeonAudio({ contextFactory: () => { created++; return null; } });
  absent.start(); absent.jump(); absent.death(); absent.finish(); absent.pause(); absent.stop();
  assert.equal(created, 0); assert.equal(await absent.unlock(), false);
  assert.equal(created, 1); assert.equal(absent.start(), false);
  const denied = new NeonAudio({ contextFactory: () => { throw new Error('device denied'); } });
  assert.equal(await denied.unlock(), false); assert.doesNotThrow(() => denied.resume());
});

test('transport restart cancels every old voice and repeats the first beat at the same relative times', async () => {
  const { audio, ctx } = await setup();
  audio.start(0);
  const first = ctx.starts.map(({ node, time }) => ({ kind: node.kind, type: node.type, time }));
  const oldVoices = Array.from(audio._voices);
  const oldBus = audio._music;
  ctx.currentTime = 9; ctx.starts = [];
  audio.start(0);
  assert.equal(audio.position, 0); assert.equal(audio.playing, true);
  assert.ok(oldBus.nodes.every(node => node.disconnected));
  assert.ok(oldVoices.every(voice => voice.nodes.every(node => node.disconnected)));
  const restarted = ctx.starts.map(({ node, time }) => ({ kind: node.kind, type: node.type, time: time - 9 }));
  assert.equal(restarted.length, first.length);
  first.forEach((source, index) => {
    assert.equal(source.kind, restarted[index].kind); assert.equal(source.type, restarted[index].type);
    assert.ok(Math.abs(source.time - restarted[index].time) < 1e-10);
  });
});

test('pause kills notes and delay tails, resume continues at the saved musical phase', async () => {
  const { audio, ctx } = await setup();
  audio.start(0); ctx.currentTime = .10; audio.update(.10); audio.jump();
  const buses = [audio._music, audio._effects];
  audio.pause();
  assert.equal(audio._voices.size, 0); assert.equal(audio.playing, false);
  assert.ok(buses.every(bus => bus.nodes.every(node => node.disconnected)));
  const previousStarts = ctx.starts.length;
  ctx.currentTime = 50; audio.update(5);
  assert.equal(ctx.starts.length, previousStarts);
  audio.resume();
  assert.equal(audio.position, .10); assert.equal(audio.playing, true);
  assert.ok(Math.abs(ctx.currentTime - audio._origin - .10 + .025) < 1e-9);
  assert.ok(audio._voices.size > 0, 'sustained chord is reconstructed on resume');
});

test('the whole track schedules finite audio and releases its voices at the finish', async () => {
  const { audio, ctx } = await setup();
  audio.start(0);
  let maximumVoices = 0;
  for (let frame = 1; frame <= 3600; frame++) {
    ctx.currentTime = frame / 60;
    audio.update(frame / 60);
    maximumVoices = Math.max(maximumVoices, audio._voices.size);
  }
  assert.equal(audio._nextEvent, SCORE.length);
  assert.equal(audio.playing, false); assert.equal(audio.position, 60);
  assert.equal(audio._voices.size, 0);
  assert.ok(maximumVoices < 160, `bounded active audio nodes: ${maximumVoices}`);
  assert.ok(ctx.starts.length > 1800);
});

test('a stalled browser resynchronizes audio to gameplay instead of advancing ahead', async () => {
  const { audio, ctx } = await setup();
  audio.start(0); ctx.currentTime = 4; audio.update(.1);
  assert.equal(audio.position, .1);
  assert.ok(Math.abs(ctx.currentTime - audio._origin - .1 + .025) < 1e-9);
});

test('music and effects mute independently, including exact zero, and short effects work', async () => {
  const { audio, ctx } = await setup();
  audio.setMusicVolume(0); audio.setEffectsVolume(.35); audio.start();
  audio.jump(); audio.death(); audio.finish();
  assert.equal(audio._music.output.gain.value, 0);
  assert.equal(audio._effects.output.gain.value, .35);
  audio.setEffectsVolume(0); audio.setMusicVolume(1.5);
  assert.equal(audio._effects.output.gain.value, 0); assert.equal(audio._music.output.gain.value, 1);
  assert.ok(ctx.starts.some(({ node }) => node.kind === 'noise'));
  audio.stop(); assert.equal(audio._voices.size, 0); assert.equal(audio.position, 0);
});

test('beat pulses and section numbers follow the same musical boundaries as the level', () => {
  const audio = new NeonAudio();
  SECTION_BEATS.forEach((beat, section) => {
    const info = audio.getBeat(beat * BEAT_SECONDS + 1e-8);
    assert.equal(info.section, section); assert.ok(info.pulse > .9999);
  });
  assert.ok(audio.getBeat(.25 * BEAT_SECONDS).pulse > audio.getBeat(.75 * BEAT_SECONDS).pulse);
});
