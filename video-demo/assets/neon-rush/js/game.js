(function () {
  'use strict';
  const $ = id => document.getElementById(id);
  const sim = new NeonCore.Simulation();
  const renderer = new NeonRenderer($('game-canvas'));
  const silent = new Proxy({}, { get: () => () => {} });
  const audio = window.NeonAudio ? new NeonAudio() : silent;
  const screens = { title: $('title-screen'), paused: $('pause-screen'), victory: $('victory-screen') };
  let state = 'title', attempts = 0, heldKeys = false, heldPointer = false, deadTime = 0;
  let lastTime = 0, menuTime = 0, lastSection = '', toastTime = 0, starting = false;
  let settingsOpen = false, settingsReturn = null, focusBeforeOverlay = null;
  let best = 0, musicVolume = 70, effectsVolume = 65;
  try {
    best = Math.max(0, Math.min(100, Number(localStorage.getItem('neonrush.best')) || 0));
    const saved = JSON.parse(localStorage.getItem('neonrush.audio') || 'null');
    if (saved) {
      if (Number.isFinite(saved.music)) musicVolume = Math.max(0, Math.min(100, saved.music));
      if (Number.isFinite(saved.effects)) effectsVolume = Math.max(0, Math.min(100, saved.effects));
    }
  } catch (_) { /* A private/file browser may disable storage; the run still works. */ }
  function saveBest() {
    best = Math.max(best, Math.floor(sim.progress * 1000) / 10);
    try { localStorage.setItem('neonrush.best', String(best)); } catch (_) {}
    $('title-best').innerHTML = `${Math.floor(best)}<span>%</span>`;
    $('hud-best').textContent = `BEST ${Math.floor(best)}%`;
  }
  function setVolume() {
    audio.setMusicVolume(musicVolume / 100); audio.setEffectsVolume(effectsVolume / 100);
    $('music-volume').value = musicVolume; $('effects-volume').value = effectsVolume;
    $('music-value').textContent = `${musicVolume}%`; $('effects-value').textContent = `${effectsVolume}%`;
    $('sound-status').textContent = musicVolume || effectsVolume ? 'SOUND ON' : 'SOUND OFF';
  }
  function clearInput() { heldKeys = false; heldPointer = false; sim.inputBuffer = 0; sim.wasHeld = false; }
  function show(next) {
    state = next;
    for (const [name, el] of Object.entries(screens)) el.hidden = name !== next;
    $('stage').classList.toggle('is-menu', next === 'title');
    $('game-hud').hidden = next === 'title';
    $('pause-button').hidden = next !== 'playing';
    $('game-tip').hidden = next !== 'playing';
    $('retry-notice').hidden = true;
    if (next === 'paused') $('resume-button').focus({ preventScroll: true });
    if (next === 'victory') $('replay-button').focus({ preventScroll: true });
  }
  function resetAttempt() {
    saveBest(); attempts++; sim.reset(); renderer.clear(); deadTime = 0; lastSection = '';
    $('attempt').textContent = String(attempts).padStart(2, '0');
    $('section-toast').classList.remove('visible'); toastTime = 0;
    show('playing'); audio.start(0); updateHUD();
  }
  async function startGame() {
    if (starting || settingsOpen) return;
    starting = true; $('play-button').disabled = true;
    try {
      if (await audio.unlock() === false) $('sound-status').textContent = 'SOUND UNAVAILABLE';
    } catch (_) { $('sound-status').textContent = 'SOUND UNAVAILABLE'; }
    attempts = 0; clearInput(); resetAttempt(); lastTime = performance.now();
    $('game-canvas').focus({ preventScroll: true });
    $('play-button').disabled = false; starting = false;
    $('announcer').textContent = 'Afterglow. Jump with Space, click or tap. Hold to jump again.';
    if (document.hidden) pause('Your run is safe. The tab was hidden.');
  }
  function pause(reason = 'The skyline can wait.') {
    if (state !== 'playing') return;
    saveBest(); clearInput(); audio.pause(); show('paused'); $('pause-reason').textContent = reason;
  }
  function resume() {
    if (state !== 'paused' || settingsOpen) return;
    clearInput(); show('playing'); lastTime = performance.now();
    if (sim.status === 'running') audio.resume(sim.elapsed);
    $('game-canvas').focus({ preventScroll: true });
  }
  function restart() {
    if (state === 'title' || settingsOpen) return;
    clearInput(); resetAttempt(); lastTime = performance.now(); $('game-canvas').focus({ preventScroll: true });
  }
  function menu() {
    saveBest(); clearInput(); audio.stop(); renderer.clear(); show('title'); $('play-button').focus({ preventScroll: true });
  }
  function openSettings() {
    if (settingsOpen) return;
    focusBeforeOverlay = document.activeElement;
    if (state === 'playing') pause();
    settingsReturn = state; settingsOpen = true; $('settings-screen').hidden = false;
    $('music-volume').focus({ preventScroll: true });
  }
  function closeSettings() {
    settingsOpen = false; $('settings-screen').hidden = true;
    if (settingsReturn === 'paused') $('resume-button').focus({ preventScroll: true });
    else if (focusBeforeOverlay) focusBeforeOverlay.focus({ preventScroll: true });
  }
  function updateHUD() {
    const pct = Math.min(100, Math.floor(sim.progress * 100));
    $('progress-text').innerHTML = `${pct}<span>%</span>`;
    $('progress-fill').style.width = `${sim.progress * 100}%`;
    $('progress-bar').setAttribute('aria-valuenow', pct);
    const section = sim.section;
    $('section-name').textContent = `${section.label} / ${section.name.toUpperCase()}`;
    if (lastSection !== section.name) {
      if (lastSection) { $('section-toast').textContent = `${section.label} / ${section.name.toUpperCase()}`; $('section-toast').classList.add('visible'); toastTime = 2.5; }
      lastSection = section.name;
    }
    $('game-tip').style.opacity = sim.elapsed < 4 ? '1' : '0';
    const pulse = Math.pow(1 - (sim.elapsed / NeonCore.constants.BEAT) % 1, 3);
    document.querySelector('.beat-dot').style.opacity = 0.35 + 0.65 * pulse;
  }
  function frame(now) {
    const dt = lastTime ? Math.min(0.1, Math.max(0, (now - lastTime) / 1000)) : 0;
    lastTime = now;
    if (state === 'title' && !document.hidden) menuTime += dt;
    if (state === 'playing') {
      if (sim.status === 'running') {
        const events = sim.advance(dt, heldKeys || heldPointer);
        audio.update(sim.elapsed);
        for (const e of events) {
          renderer.event(e.type, sim);
          if (e.type === 'jump') audio.jump();
          if (e.type === 'death') {
            saveBest(); audio.stop(); audio.death(); deadTime = 0;
            $('death-progress').textContent = `${Math.floor(sim.progress * 100)}%`; $('retry-notice').hidden = false;
          }
          if (e.type === 'finish') {
            saveBest(); clearInput(); audio.stop(); audio.finish();
            $('finish-attempts').textContent = String(attempts).padStart(2, '0');
            show('victory'); $('announcer').textContent = `Level complete in ${attempts} attempts. One hundred percent.`;
          }
        }
        updateHUD();
      } else if (sim.status === 'dead') {
        deadTime += dt;
        if (deadTime >= 0.27) resetAttempt();
      }
      if (toastTime > 0) { toastTime -= dt; if (toastTime <= 0) $('section-toast').classList.remove('visible'); }
    }
    if (state !== 'paused' && !settingsOpen) renderer.update(dt, sim, state === 'playing');
    renderer.render(sim, state, menuTime);
    requestAnimationFrame(frame);
  }
  $('play-button').addEventListener('click', startGame);
  $('replay-button').addEventListener('click', startGame);
  $('pause-button').addEventListener('click', () => pause());
  $('resume-button').addEventListener('click', resume);
  $('restart-button').addEventListener('click', restart);
  $('quit-button').addEventListener('click', menu);
  $('finish-menu-button').addEventListener('click', menu);
  $('brand').addEventListener('click', event => { event.preventDefault(); if (state === 'playing') pause(); else if (!settingsOpen) menu(); });
  $('settings-button').addEventListener('click', openSettings);
  $('close-settings').addEventListener('click', closeSettings);
  $('settings-done').addEventListener('click', closeSettings);
  for (const name of ['music', 'effects']) $(name + '-volume').addEventListener('input', event => {
    if (name === 'music') musicVolume = Number(event.target.value); else effectsVolume = Number(event.target.value);
    setVolume();
    try { localStorage.setItem('neonrush.audio', JSON.stringify({ music: musicVolume, effects: effectsVolume })); } catch (_) {}
  });
  document.addEventListener('keydown', event => {
    if (event.code === 'Tab') {
      const active = settingsOpen ? $('settings-screen') : state === 'paused' ? $('pause-screen') : state === 'victory' ? $('victory-screen') : null;
      if (active) {
        const controls = Array.from(active.querySelectorAll('button, input'));
        const first = controls[0], last = controls[controls.length - 1];
        if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
        else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
      }
      return;
    }
    if (event.code === 'Escape') {
      event.preventDefault(); if (event.repeat) return;
      if (settingsOpen) closeSettings(); else if (state === 'playing') pause(); else if (state === 'paused') resume(); return;
    }
    if (settingsOpen || event.target.tagName === 'INPUT') return;
    if (event.code === 'KeyR' && !event.repeat) { event.preventDefault(); restart(); return; }
    if (event.code === 'Space' || event.code === 'ArrowUp') {
      if (event.code === 'Space' && event.target.tagName === 'BUTTON') return;
      event.preventDefault();
      if (state === 'title' || state === 'victory') { if (!event.repeat) startGame(); }
      else if (state === 'playing') {
        heldKeys = true;
        // Preserve a quick key tap even when keyup arrives before the frame.
        if (!event.repeat) sim.inputBuffer = 0.09;
      }
    }
  });
  document.addEventListener('keyup', event => { if (event.code === 'Space' || event.code === 'ArrowUp') heldKeys = false; });
  $('game-canvas').addEventListener('pointerdown', event => {
    if (event.button !== 0 || state !== 'playing' || settingsOpen) return;
    event.preventDefault(); heldPointer = true;
    // Buffer a tap even if it is released before the next animation frame.
    sim.inputBuffer = 0.09;
    $('game-canvas').setPointerCapture(event.pointerId);
  });
  for (const eventName of ['pointerup', 'pointercancel', 'lostpointercapture']) window.addEventListener(eventName, () => { heldPointer = false; });
  window.addEventListener('blur', () => pause('Your run is safe. Come back when you’re ready.'));
  document.addEventListener('visibilitychange', () => { if (document.hidden) pause('Your run is safe. The tab was hidden.'); });
  window.addEventListener('resize', () => renderer.resize());
  $('game-canvas').addEventListener('contextmenu', event => event.preventDefault());
  setVolume(); saveBest(); show('title'); requestAnimationFrame(frame);
})();
