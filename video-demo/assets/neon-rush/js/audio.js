/* Neon Rush — "Afterglow Circuit", an original 140 BPM synthwave score.
 * Classic script, no requests or assets: works when index.html is opened locally.
 * The arrangement is declarative so its timing and transport can be tested.
 */
(function (root, factory) {
  'use strict';
  const api = factory(root);
  if (typeof module === 'object' && module.exports) module.exports = api;
  if (root) root.NeonAudio = api.NeonAudio;
})(typeof globalThis !== 'undefined' ? globalThis : this, function (root) {
  'use strict';

  const BPM = 140;
  const BEAT_SECONDS = 60 / BPM;
  const TOTAL_BEATS = 140;
  const DURATION_SECONDS = TOTAL_BEATS * BEAT_SECONDS;
  const EPSILON = 0.0001;
  const SECTION_BEATS = Object.freeze([0, 32, 64, 96, 124]);
  const clamp = (value, low, high) => Math.max(low, Math.min(high, Number.isFinite(value) ? value : low));
  const frequency = note => 440 * Math.pow(2, (note - 69) / 12);

  // F# minor, Dmaj7, Aadd9, E. The voice-leading keeps two notes close
  // between chords, while the bass establishes a different root each bar.
  const HARMONY = [
    { root: 42, notes: [54, 57, 61, 68], arp: [66, 69, 73, 80] },
    { root: 38, notes: [50, 57, 61, 66], arp: [62, 69, 73, 78] },
    { root: 45, notes: [52, 57, 61, 71], arp: [64, 69, 73, 83] },
    { root: 40, notes: [52, 56, 59, 66], arp: [64, 68, 71, 78] }
  ];

  // Eight bars, with space for the hook to breathe. Tuples are beat, MIDI,
  // length in beats; the second half answers the first half an octave below.
  const HOOK = [
    [[0, 78, .70], [1, 81, .45], [1.75, 85, 1.05], [3, 83, .70]],
    [[.25, 81, 1.15], [1.75, 78, .65], [2.75, 76, .35], [3.25, 78, .55]],
    [[0, 81, .70], [1, 85, .45], [1.75, 88, .95], [3, 85, .65]],
    [[0, 83, 1.20], [1.5, 80, .55], [2.5, 78, .40], [3.25, 76, .50]],
    [[0, 78, .65], [.75, 81, .40], [1.5, 85, .90], [2.75, 83, .35], [3.25, 81, .50]],
    [[.25, 78, 1.35], [2, 76, .40], [2.75, 73, .80]],
    [[0, 76, .65], [1, 78, .45], [1.75, 81, .95], [3, 78, .70]],
    [[0, 80, .90], [1.25, 78, .50], [2, 76, .45], [2.75, 73, .40], [3.5, 76, .35]]
  ];

  function composeScore() {
    const events = [];
    const add = (type, beat, duration, note, velocity, pan) => {
      if (beat >= TOTAL_BEATS) return;
      events.push(Object.freeze({
        type, beat, duration: Math.min(duration, TOTAL_BEATS - beat),
        note: note === undefined ? 0 : note,
        velocity: velocity === undefined ? 1 : velocity,
        pan: pan || 0
      }));
    };
    for (let bar = 0; bar < 35; bar++) {
      const beat = bar * 4;
      const chord = HARMONY[bar % 4];
      const intro = bar < 8;
      const drive = bar >= 8;
      const climb = bar >= 16;
      const finale = bar >= 24;
      const finalRun = bar >= 31;

      // Four-on-the-floor, with tiny deterministic accents rather than random
      // timing. Rhythm is exact; texture comes from changing orchestration.
      for (let n = 0; n < 4; n++) {
        add('kick', beat + n, .72, 0, n === 0 ? 1 : .89);
        if (bar >= 2 && n % 2 === 1) add('clap', beat + n, .48, 0, .78);
        if (bar >= 1) {
          add('hat', beat + n + .5, .22, 0, .57, n % 2 ? -.24 : .24);
          if (drive) add('hat', beat + n, .12, 0, .28, -.20);
          if (finale && n >= 2) add('hat', beat + n + .75, .12, 0, .32, .26);
        }
      }
      if ([0, 8, 16, 24, 31].includes(bar)) add('crash', beat, 3.5, 0, bar === 0 ? .30 : .51);

      // Pads swell around the drums, and every four bars the inversion lifts.
      chord.notes.forEach((note, voice) => add('pad', beat, 3.90, note,
        intro ? .36 : .48, (voice - 1.5) * .25));

      if (bar < 2) {
        add('bass', beat, 1.45, chord.root, .62);
        add('bass', beat + 2, 1.3, chord.root, .58);
      } else {
        const bassPattern = finalRun
          ? [[0, 0], [.5, 0], [1, 12], [1.5, 0], [2, 0], [2.5, 7], [3, 12], [3.5, 0], [3.75, 12]]
          : [[0, 0], [.5, 0], [1.25, 12], [1.75, 0], [2, 0], [2.75, 7], [3.25, 12], [3.5, 0]];
        bassPattern.forEach(([offset, octave], n) => add('bass', beat + offset,
          finalRun && n === 8 ? .20 : .36, chord.root + octave, n % 2 ? .65 : .80));
      }

      const arpOrder = [0, 2, 1, 3, 2, 1, 3, 2];
      const arpRate = finalRun ? .25 : .5;
      const arpCount = bar < 2 ? 4 : finalRun ? 16 : 8;
      for (let n = 0; n < arpCount; n++) {
        const offset = bar < 2 ? n : n * arpRate;
        add('arp', beat + offset, finalRun ? .31 : .48,
          chord.arp[arpOrder[n % 8]], intro ? .38 : finale ? .53 : .43,
          n % 2 ? -.34 : .34);
      }

      // The melody is previewed softly in the intro, sung in full at the
      // first drop, and doubled at the climax. A new last bar resolves it.
      if (bar >= 4 && bar !== 34) {
        const phrase = HOOK[bar % 8];
        phrase.forEach(([offset, note, duration]) => {
          add('lead', beat + offset, duration, intro ? note - 12 : note,
            intro ? .34 : finale ? .72 : .63, -.06);
          if (finale && duration >= .65) add('counter', beat + offset, duration,
            note - 12, .27, .22);
        });
      }
      if (climb && bar % 4 === 3) {
        // Short answering line, not another full-time layer over the hook.
        [73, 76, 80, 83].forEach((note, n) => add('counter', beat + 2 + n * .5, .35, note,
          .36, n % 2 ? -.28 : .28));
      }
      if ([7, 15, 23, 30, 33].includes(bar)) {
        add('riser', beat + 2, 1.97, 0, .40);
        [.0, .5, .75].forEach((offset, n) => add('clap', beat + 3 + offset, .20, 0, .28 + n * .14));
      }
      if (bar === 34) {
        [[0, 85, .45], [.5, 83, .45], [1, 81, .65], [2, 78, 1.85]].forEach(([offset, note, duration]) =>
          add('lead', beat + offset, duration, note, .79));
      }
    }
    return Object.freeze(events.sort((a, b) => a.beat - b.beat));
  }
  const SCORE = composeScore();

  class NeonAudio {
    constructor(options) {
      const opts = options || {};
      this.context = null;
      this.available = true;
      this.musicVolume = .68;
      this.effectsVolume = .70;
      this.playing = false;
      this.position = 0;
      this._contextFactory = opts.contextFactory || (() => {
        const AudioContext = root.AudioContext || root.webkitAudioContext;
        return AudioContext ? new AudioContext({ latencyHint: 'interactive' }) : null;
      });
      this._voices = new Set();
      this._music = null;
      this._effects = null;
      this._origin = 0;
      this._nextEvent = 0;
      this._noise = null;
      this._master = null;
    }

    async unlock() {
      try {
        if (!this.context) {
          this.context = this._contextFactory();
          if (!this.context) { this.available = false; return false; }
          this._initialize();
        }
        if (this.context.state === 'suspended') await this.context.resume();
        this.available = this.context.state !== 'closed';
        return this.available;
      } catch (_) {
        // Gameplay remains available when a browser or device denies audio.
        this.available = false;
        return false;
      }
    }

    _initialize() {
      const ctx = this.context;
      this._master = ctx.createDynamicsCompressor();
      this._master.threshold.value = -13;
      this._master.knee.value = 12;
      this._master.ratio.value = 3.5;
      this._master.attack.value = .005;
      this._master.release.value = .16;
      const output = ctx.createGain();
      output.gain.value = .68;
      this._master.connect(output);
      output.connect(ctx.destination);
      this._noise = ctx.createBuffer(1, ctx.sampleRate * 2, ctx.sampleRate);
      const channel = this._noise.getChannelData(0);
      let seed = 0x4e454f4e;
      for (let i = 0; i < channel.length; i++) {
        // Repeatable texture, including identical sound on every restart.
        seed ^= seed << 13; seed ^= seed >>> 17; seed ^= seed << 5;
        channel[i] = (seed >>> 0) / 2147483648 - 1;
      }
    }

    _createBus(kind) {
      const ctx = this.context;
      const input = ctx.createGain();
      const output = ctx.createGain();
      output.gain.value = kind === 'music' ? this.musicVolume : this.effectsVolume;
      input.connect(output);
      output.connect(this._master);
      const bus = { input, output, nodes: [input, output], send: null };
      if (kind === 'music') {
        const send = ctx.createGain();
        const delay = ctx.createDelay(1);
        const feedback = ctx.createGain();
        const filter = ctx.createBiquadFilter();
        const wet = ctx.createGain();
        delay.delayTime.value = BEAT_SECONDS * .75;
        feedback.gain.value = .27;
        filter.type = 'lowpass';
        filter.frequency.value = 3300;
        wet.gain.value = .17;
        send.connect(delay);
        delay.connect(filter);
        filter.connect(feedback);
        feedback.connect(delay);
        filter.connect(wet);
        wet.connect(output);
        bus.send = send;
        bus.nodes.push(send, delay, feedback, filter, wet);
      }
      this[kind === 'music' ? '_music' : '_effects'] = bus;
      return bus;
    }

    _kill(kind) {
      for (const voice of Array.from(this._voices)) {
        if (voice.kind !== kind) continue;
        for (const source of voice.sources) {
          try { source.stop(this.context.currentTime); } catch (_) { /* already ended */ }
        }
        this._disposeVoice(voice);
      }
      const name = kind === 'music' ? '_music' : '_effects';
      const bus = this[name];
      if (bus) bus.nodes.forEach(node => { try { node.disconnect(); } catch (_) { /* disconnected */ } });
      this[name] = null;
    }

    _disposeVoice(voice) {
      if (!this._voices.has(voice)) return;
      this._voices.delete(voice);
      voice.nodes.forEach(node => { try { node.disconnect(); } catch (_) { /* disconnected */ } });
    }

    _register(kind, sources, nodes, end) {
      const voice = { kind, sources, nodes, end };
      this._voices.add(voice);
      let outstanding = sources.length;
      sources.forEach(source => { source.onended = () => { if (--outstanding === 0) this._disposeVoice(voice); }; });
      return voice;
    }

    start(offsetSeconds) {
      if (!this.context || !this.available || !this._master) return false;
      this._kill('music');
      this._kill('effects');
      this.position = clamp(offsetSeconds || 0, 0, DURATION_SECONDS);
      this.playing = this.position < DURATION_SECONDS;
      if (!this.playing) return false;
      this._createBus('music');
      this._origin = this.context.currentTime + .025 - this.position;
      this._nextEvent = SCORE.findIndex(event => event.beat * BEAT_SECONDS >= this.position - 1e-7);
      if (this._nextEvent < 0) this._nextEvent = SCORE.length;
      if (this.position > 0) {
        SCORE.forEach(event => {
          const begin = event.beat * BEAT_SECONDS;
          const end = begin + event.duration * BEAT_SECONDS;
          if (['pad', 'lead', 'counter', 'bass'].includes(event.type) && begin < this.position && end > this.position + .025) {
            this._schedule(event, this.context.currentTime + .025, end - this.position);
          }
        });
      }
      this.update(this.position);
      return true;
    }

    pause() {
      this.playing = false;
      if (!this.context) return;
      this._kill('music');
      this._kill('effects');
    }

    resume(offsetSeconds) {
      return this.start(offsetSeconds === undefined ? this.position : offsetSeconds);
    }

    stop() {
      this.pause();
      this.position = 0;
      this._nextEvent = 0;
    }

    update(gameSeconds) {
      if (!this.playing || !this.context) return;
      const position = clamp(gameSeconds, 0, DURATION_SECONDS);
      const ctx = this.context;
      this.position = position;
      if (position >= DURATION_SECONDS) { this.pause(); return; }
      // If the browser stalls, recover to the simulation instead of allowing
      // the music to run ahead of obstacles. No independent timer is required.
      const drift = ctx.currentTime - this._origin - position;
      if (Math.abs(drift) > .16) { this.start(position); return; }
      const horizon = Math.min(DURATION_SECONDS, position + .16);
      while (this._nextEvent < SCORE.length && SCORE[this._nextEvent].beat * BEAT_SECONDS <= horizon) {
        const event = SCORE[this._nextEvent++];
        const scheduled = this._origin + event.beat * BEAT_SECONDS;
        if (scheduled >= ctx.currentTime - .04) this._schedule(event, Math.max(ctx.currentTime + .002, scheduled));
      }
      for (const voice of Array.from(this._voices)) {
        if (voice.end + .03 < ctx.currentTime) this._disposeVoice(voice);
      }
    }

    setMusicVolume(value) {
      this.musicVolume = clamp(value, 0, 1);
      if (this._music) this._music.output.gain.setTargetAtTime(this.musicVolume, this.context.currentTime, .02);
    }

    setEffectsVolume(value) {
      this.effectsVolume = clamp(value, 0, 1);
      if (this._effects) this._effects.output.gain.setTargetAtTime(this.effectsVolume, this.context.currentTime, .02);
    }

    getBeat(gameSeconds) {
      const beat = clamp(gameSeconds, 0, DURATION_SECONDS) / BEAT_SECONDS;
      const phase = beat % 1;
      let section = 0;
      SECTION_BEATS.forEach((start, index) => { if (beat >= start) section = index; });
      return { beat, phase, pulse: Math.exp(-phase * 7), section };
    }

    _envelope(gain, time, duration, peak, attack, release, sustain) {
      const param = gain.gain;
      const end = time + Math.max(duration, attack + release + .004);
      param.setValueAtTime(EPSILON, time);
      param.linearRampToValueAtTime(Math.max(EPSILON, peak), time + attack);
      param.exponentialRampToValueAtTime(Math.max(EPSILON, peak * sustain), Math.max(time + attack + .001, end - release));
      param.exponentialRampToValueAtTime(EPSILON, end);
      return end;
    }

    _tone(kind, time, duration, note, velocity, settings) {
      const ctx = this.context;
      const bus = kind === 'music' ? this._music : (this._effects || this._createBus('effects'));
      if (!bus) return;
      const env = ctx.createGain();
      const filter = ctx.createBiquadFilter();
      filter.type = 'lowpass';
      filter.Q.value = settings.q || .55;
      filter.frequency.setValueAtTime(settings.cutoff, time);
      filter.frequency.exponentialRampToValueAtTime(Math.max(90, settings.cutoff * (settings.sweep || .55)), time + duration);
      filter.connect(env);
      const nodes = [filter, env];
      let last = env;
      if (ctx.createStereoPanner) {
        const panner = ctx.createStereoPanner();
        panner.pan.value = settings.pan || 0;
        env.connect(panner); last = panner; nodes.push(panner);
      }
      last.connect(bus.input);
      if (bus.send && settings.echo) last.connect(bus.send);
      const end = this._envelope(env, time, duration, settings.level * velocity,
        settings.attack || .005, settings.release || .045, settings.sustain || .55);
      const sources = [];
      const detunes = settings.detunes || [0];
      detunes.forEach(detune => {
        const osc = ctx.createOscillator();
        osc.type = settings.wave;
        osc.detune.value = detune;
        osc.frequency.setValueAtTime(frequency(note), time);
        if (settings.slide) osc.frequency.exponentialRampToValueAtTime(frequency(note + settings.slide), end);
        osc.connect(filter); osc.start(time); osc.stop(end + .006);
        sources.push(osc); nodes.push(osc);
      });
      this._register(kind, sources, nodes, end + .006);
    }

    _noiseHit(kind, time, duration, velocity, settings) {
      const ctx = this.context;
      const bus = kind === 'music' ? this._music : (this._effects || this._createBus('effects'));
      if (!bus) return;
      const source = ctx.createBufferSource();
      source.buffer = this._noise;
      const filter = ctx.createBiquadFilter();
      filter.type = settings.type || 'highpass';
      filter.Q.value = settings.q || .65;
      filter.frequency.setValueAtTime(settings.frequency, time);
      if (settings.sweep) filter.frequency.exponentialRampToValueAtTime(settings.sweep, time + duration);
      const env = ctx.createGain();
      const nodes = [source, filter, env];
      source.connect(filter); filter.connect(env);
      let last = env;
      if (ctx.createStereoPanner) {
        const pan = ctx.createStereoPanner();
        pan.pan.value = settings.pan || 0;
        env.connect(pan); last = pan; nodes.push(pan);
      }
      last.connect(bus.input);
      const end = this._envelope(env, time, duration, velocity * settings.level,
        settings.attack || .002, settings.release || duration * .78, settings.sustain || .55);
      source.start(time, settings.offset || 0); source.stop(end + .004);
      this._register(kind, [source], nodes, end + .004);
    }

    _kick(time, velocity) {
      const ctx = this.context;
      const source = ctx.createOscillator();
      source.type = 'sine';
      source.frequency.setValueAtTime(158, time);
      source.frequency.exponentialRampToValueAtTime(47, time + .11);
      source.frequency.exponentialRampToValueAtTime(39, time + .27);
      const gain = ctx.createGain();
      this._envelope(gain, time, .31, velocity * .66, .003, .19, .65);
      source.connect(gain); gain.connect(this._music.input);
      source.start(time); source.stop(time + .32);
      this._register('music', [source], [source, gain], time + .32);
      this._noiseHit('music', time, .026, velocity, { frequency: 2200, type: 'bandpass', level: .12, release: .020 });
    }

    _schedule(event, time, remainingDuration) {
      const duration = remainingDuration || event.duration * BEAT_SECONDS;
      const velocity = event.velocity;
      switch (event.type) {
        case 'kick': this._kick(time, velocity); break;
        case 'clap':
          [0, .012, .025].forEach((offset, index) => this._noiseHit('music', time + offset,
            index === 2 ? .14 : .028, velocity, { type: 'bandpass', frequency: 1650, q: .72,
              level: index === 2 ? .18 : .14, offset: .14 + index * .11, release: index === 2 ? .11 : .020 }));
          break;
        case 'hat': this._noiseHit('music', time, duration, velocity,
          { frequency: 7100, level: .115, pan: event.pan, offset: .65 }); break;
        case 'crash': this._noiseHit('music', time, duration, velocity,
          { frequency: 5100, sweep: 3000, level: .18, attack: .009, release: duration * .88, offset: .02 }); break;
        case 'riser': this._noiseHit('music', time, duration, velocity,
          { frequency: 650, sweep: 9200, type: 'bandpass', level: .20, attack: duration * .82,
            release: .10, sustain: .96, pan: .12 }); break;
        case 'bass': this._tone('music', time, duration, event.note, velocity,
          { wave: 'sawtooth', cutoff: 680, sweep: .28, level: .19, attack: .012,
            release: .045, sustain: .45, detunes: [0] });
          this._tone('music', time, duration, event.note - 12, velocity,
            { wave: 'sine', cutoff: 220, level: .115, attack: .009, release: .04, sustain: .65 }); break;
        case 'pad': this._tone('music', time, duration, event.note, velocity,
          { wave: 'triangle', cutoff: 2600, sweep: .7, level: .064, attack: Math.min(.22, duration * .2),
            release: Math.min(.24, duration * .3), sustain: .8, detunes: [-7, 7], pan: event.pan, echo: true }); break;
        case 'arp': this._tone('music', time, duration, event.note, velocity,
          { wave: 'triangle', cutoff: 7100, sweep: .24, level: .14, attack: .003,
            release: .065, sustain: .26, pan: event.pan, echo: true }); break;
        case 'lead': this._tone('music', time, duration, event.note, velocity,
          { wave: 'sawtooth', cutoff: 3900, sweep: .60, level: .076, attack: .012,
            release: .055, sustain: .7, detunes: [-5, 5], pan: event.pan, echo: true }); break;
        case 'counter': this._tone('music', time, duration, event.note, velocity,
          { wave: 'square', cutoff: 1900, sweep: .7, level: .042, attack: .008,
            release: .05, sustain: .56, pan: event.pan, echo: true }); break;
      }
    }

    jump() {
      if (!this.context || !this.available) return;
      this._tone('effects', this.context.currentTime + .002, .105, 78, .75,
        { wave: 'sine', cutoff: 6500, level: .105, attack: .003, release: .075, sustain: .3, slide: 9 });
    }

    death() {
      if (!this.context || !this.available) return;
      const time = this.context.currentTime + .002;
      this._noiseHit('effects', time, .25, .76,
        { frequency: 1900, sweep: 220, type: 'lowpass', level: .23, release: .21 });
      this._tone('effects', time, .25, 48, .8,
        { wave: 'triangle', cutoff: 1400, level: .20, attack: .002, release: .19, sustain: .35, slide: -17 });
    }

    finish() {
      if (!this.context || !this.available) return;
      const time = this.context.currentTime + .005;
      [66, 69, 73, 78, 81, 85].forEach((note, index) => this._tone('effects', time + index * .075,
        .65, note, .70, { wave: 'triangle', cutoff: 6400, level: .16, attack: .004,
          release: .4, sustain: .6, pan: (index % 2 ? 1 : -1) * .18 }));
      this._tone('effects', time, 1.05, 42, .8,
        { wave: 'sine', cutoff: 650, level: .19, attack: .018, release: .65, sustain: .7 });
    }
  }

  return { NeonAudio, SCORE, BPM, BEAT_SECONDS, TOTAL_BEATS, DURATION_SECONDS, SECTION_BEATS };
});
