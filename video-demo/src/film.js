(() => {
  const tl = gsap.timeline({ paused: true });
  const smooth = "power3.inOut";
  const reveal = "power3.out";
  const $ = (selector) => document.querySelector(selector);
  const $$ = (selector) => [...document.querySelectorAll(selector)];
  const T = DELM_TIMING, film = DELM_FILM;
  const typing = Object.fromEntries(DELM_TYPING.map((field) => [field.name, field]));
  const clamp = (value) => Math.max(0, Math.min(1, value));
  const progress = (time, start, duration) => clamp((time - start) / duration);
  const easeInOut = (p) => p < .5 ? 4 * p * p * p : 1 - (-2 * p + 2) ** 3 / 2;
  const smoothstep = (p) => p * p * (3 - 2 * p);
  const typed = (name, t) => {
    const field = typing[name];
    return field.text.slice(0, field.times.filter((at) => at <= t).length);
  };
  const escape = (text) => String(text).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

  $(".copy-command").textContent = film.installer;

  // Window geometry: a compact installer terminal, then a full Claude Code window.
  const INSTALL_WINDOW = { width: 1280, height: 700 };
  const CLAUDE_WINDOW = { left: 96, top: 80, width: 1728, height: 920 };
  const BAR = 44;
  const windowEase = gsap.parseEase(smooth);
  // The close framing keeps the whole Claude Code window in view, edge to edge.
  const AGENTS = { scale: 1.09, x: -86.4, y: -48.6 };

  const show = (selector, start, duration = .5) =>
    tl.fromTo(selector, { opacity: 0 }, { opacity: 1, duration, ease: reveal }, start);
  // Explicit start values keep every frame identical however the renderer seeks.
  const hide = (selector, start, duration = .4) =>
    tl.fromTo(selector, { opacity: 1 }, { opacity: 0, duration, ease: "power2.in", immediateRender: false }, start);
  const enter = (selector, start, y = 24, duration = .65) =>
    tl.fromTo(selector, { opacity: 0, y }, { opacity: 1, y: 0, duration, ease: reveal }, start);

  // Title
  tl.to("#cover", { y: -18, opacity: 0, duration: .35, ease: smooth }, T.title.out);

  // Installation in a terminal that settles square before typing begins.
  tl.fromTo("#term-rig", { y: 36 }, { y: 0, duration: 1.05, ease: reveal }, T.install.window_in);
  tl.fromTo("#term-rig", { opacity: 0 }, { opacity: 1, duration: .3, ease: "power1.out" }, T.install.window_in);
  tl.fromTo("#term", { rotationX: 1.5, rotationY: -2 }, { rotationX: 0, rotationY: 0, duration: 1.05, ease: "power2.out" }, T.install.window_in);
  enter("#opening-title", T.install.window_in + .12, 28, .85);

  // The same window grows into Claude Code.
  const launch = T.claude.expand;
  tl.to("#opening-title", { y: -80, opacity: 0, duration: .72, ease: smooth }, launch);
  tl.to("#term-rig", { ...CLAUDE_WINDOW, duration: T.claude.expand_duration, ease: smooth }, launch);
  tl.to("#install-screen", { opacity: 0, y: -24, duration: .3, ease: smooth }, launch + .05);
  show("#claude-screen", T.claude.screen, .35);

  // Hold on the board, then move in as both agents appear beside it.
  tl.to("#camera", { ...AGENTS, duration: T.camera.push[1], ease: smooth }, T.camera.push[0]);
  tl.fromTo(".agent", { opacity: 0, y: 22 }, { opacity: 1, y: 0, duration: .6, ease: reveal, stagger: .14 }, T.agents.in);
  tl.to(".agent", { opacity: 0, y: -10, duration: .45, ease: "power2.in" }, T.agents.out);

  // The result: the camera pushes into Claude's message and the game opens out of it.
  const R = T.result;
  // Zoom in place on the message so the terminal always covers the frame.
  const MESSAGE_POINT = [573, 478];
  const onScreen = [MESSAGE_POINT[0] * AGENTS.scale + AGENTS.x, MESSAGE_POINT[1] * AGENTS.scale + AGENTS.y];
  const ZOOM = { scale: 1.6, x: onScreen[0] - MESSAGE_POINT[0] * 1.6, y: onScreen[1] - MESSAGE_POINT[1] * 1.6 };
  // The game grows out of the message's position on screen (#game sits at 80, 45).
  const gameOrigin = `${(onScreen[0] - 80).toFixed(1)}px ${(onScreen[1] - 45).toFixed(1)}px`;
  tl.to("#camera", { ...ZOOM, duration: R.zoom[1], ease: "power2.in" }, R.zoom[0]);
  tl.fromTo("#game-backdrop", { opacity: 0 }, { opacity: 1, duration: .45, ease: "power1.in" }, R.game - .25);
  tl.set("#camera", { opacity: 0 }, R.game + .3);
  tl.fromTo("#game", { scale: .16, opacity: 0, transformOrigin: gameOrigin },
    { scale: 1, opacity: 1, duration: .9, ease: "expo.out", transformOrigin: gameOrigin }, R.game);
  tl.fromTo("#pointer", { x: 620, y: 880, opacity: 0 },
    { x: 398, y: 675, opacity: 1, duration: .45, ease: smooth, immediateRender: false }, R.play_click - .55);
  tl.to("#pointer", { scale: .85, duration: .08 }, R.play_click).to("#pointer", { scale: 1, duration: .16 }, R.play_click + .08);
  hide("#pointer", R.play_click + .3, .3);
  tl.fromTo("#callout-1", { opacity: 0, y: 16, scale: .9, xPercent: -50 },
    { opacity: 1, y: 0, scale: 1, xPercent: -50, duration: .5, ease: "back.out(1.8)", transformOrigin: "50% 0%" }, R.callouts[0]);
  tl.fromTo("#callout-2", { opacity: 0, y: 16, scale: .9 },
    { opacity: 1, y: 0, scale: 1, duration: .5, ease: "back.out(1.8)", transformOrigin: "88% 0%" }, R.callouts[1]);
  // Everything leaves before the footage ends, then the night gives way to the paper.
  hide("#callout-1", R.exit - .1, .3);
  hide("#callout-2", R.exit - .1, .3);
  tl.fromTo("#game", { scale: 1, opacity: 1, transformOrigin: "50% 50%" },
    { scale: .94, opacity: 0, duration: .55, ease: "power2.in", transformOrigin: "50% 50%", immediateRender: false }, R.exit);
  hide("#game-backdrop", R.exit + .2, .6);

  // Closing: the original choreography, then both mascots hop onto the command.
  const C = T.closing;
  show("#closing", C.in, .4);
  enter("#closing-title .closing-line", C.title, 24, .7);
  enter("#closing-command", C.command, 14, .55);
  tl.fromTo("#closing-clawd", { opacity: 0, y: 26, scale: .55 },
    { opacity: 1, y: 0, scale: 1, duration: .5, ease: "back.out(2.2)", transformOrigin: "50% 100%" }, C.mascots);
  tl.fromTo("#closing-codex", { opacity: 0, y: 26, scale: .55 },
    { opacity: 1, y: 0, scale: 1, duration: .5, ease: "back.out(2.2)", transformOrigin: "50% 100%" }, C.mascots + .16);
  tl.to({}, { duration: T.duration }, 0);

  // Installation transcript. Lines appear as the installer prints them.
  const I = T.install;
  const caret = '<b class="typed-caret"></b>';
  const shell = (name, caretUntil) => (t) =>
    `<span class="path">${escape(film.project)}</span> <span class="prompt-symbol">$</span> ${escape(typed(name, t))}` +
    (t < caretUntil && (t >= typing[name].times[0] || Math.floor(t * 2.3) % 2 === 0) ? caret : "");
  const SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";
  const install = (host, [start, done]) => (t) => t < done
    ? `<span class="spin">${SPINNER[Math.floor(t * 12) % SPINNER.length]}</span> Installing DeLM in ${host}…`
    : `<span class="ok">✓</span> Installed DeLM in ${host}`;
  const logo = [" ____       _     __  __", "|  _ \\  ___| |   |  \\/  |", "| | | |/ _ \\ |   | |\\/| |", "| |_| |  __/ |___| |  | |", "|____/ \\___|_____|_|  |_|"];
  const installLines = [
    { at: I.window_in + .5, html: shell("installer", I.enter) },
    ...logo.map((text, index) => ({ at: I.logo + index * .03, html: escape(text), cls: "logo" })),
    { at: I.logo + .16, html: "" },
    { at: I.menu, html: "Choose where to manage DeLM:" },
    { at: I.menu + .03, html: "  1. Codex" },
    { at: I.menu + .05, html: "  2. Claude Code" },
    { at: I.menu + .07, html: "  3. Both" },
    { at: I.menu + .07, html: "" },
    { at: I.menu + .1, html: (t) => `Choose 1, 2, or 3: ${escape(typed("choice", t))}` + (t < I.choice_enter ? caret : "") },
    { at: I.claude[0], html: install("Claude Code", I.claude) },
    { at: I.codex[0], html: install("Codex", I.codex) },
    { at: I.ready, html: "" },
    { at: I.ready, html: '<span class="brand">DeLM</span> <span class="ready">is ready in Claude Code and Codex.</span>' },
    { at: I.shell, html: shell("launch", I.launch_enter + 1) },
  ];
  const installContainer = $("#install-lines");
  installContainer.innerHTML = installLines.map((line) => `<div class="install-line ${line.cls || ""}"></div>`).join("");
  const installElements = [...installContainer.children];

  function updateInstall(t) {
    installLines.forEach((line, index) => {
      const element = installElements[index];
      const visible = t >= line.at;
      element.style.display = visible ? "block" : "none";
      if (!visible) return;
      const html = typeof line.html === "function" ? line.html(t) : line.html;
      if (element.innerHTML !== html) element.innerHTML = html;
    });
    $("#term-caption").textContent = t < T.claude.expand ? "neon-rush — -zsh" : "neon-rush — claude";
  }

  // The host name turns once, upward, from Claude Code to Codex.
  // Rendered widths of each name and mascot in Geist 112 px; #host-slot uses the wider one.
  const HOST_WIDTHS = { claude: 810, codex: 466 };
  const title = $("#cover-title"), claudeHost = $("#host-claude"), codexHost = $("#host-codex");
  // Extra room on the right lets a raised arm wave without being clipped;
  // the matching negative margin keeps the title centered on the names.
  const WAVE_ROOM = 48;
  $("#host-slot").style.width = `${HOST_WIDTHS.claude + WAVE_ROOM}px`;
  $("#host-slot").style.marginRight = `${-WAVE_ROOM}px`;
  function updateTitle(t) {
    const p = progress(t, T.title.roll, T.title.roll_duration);
    // A wheel accelerates, passes its stop slightly, and settles.
    const turn = easeInOut(p) + .045 * Math.sin(Math.PI * clamp((p - .55) / .45));
    const height = 200;
    claudeHost.style.transform = `translateY(${(-turn * height).toFixed(2)}px)`;
    codexHost.style.transform = `translateY(${((1 - turn) * height).toFixed(2)}px)`;
    const blur = (Math.sin(Math.PI * p) * 2.4).toFixed(2);
    claudeHost.style.filter = codexHost.style.filter = `blur(${blur}px)`;
    const width = HOST_WIDTHS.claude + (HOST_WIDTHS.codex - HOST_WIDTHS.claude) * easeInOut(p);
    title.style.transform = `translateX(${((HOST_WIDTHS.claude - width) / 2).toFixed(2)}px)`;
  }

  // Mascots wave, blink, and breathe on the film clock.
  const M = C.mascots;
  const mascots = [
    { root: "#host-claude", arm: -55, swing: 16, rate: 1.6, length: 1.08, wave: T.title.clawd_wave, blinks: [1.15], phase: 0 },
    { root: "#host-codex", arm: -120, swing: 18, rate: 2.4, length: 1.35, wave: T.title.codex_wave, blinks: [2.95], phase: 1.3 },
    { root: "#closing-clawd", arm: -55, swing: 16, rate: 1.6, length: 1.7, wave: M + .55, blinks: [M + 2.1, M + 3.5], phase: .4 },
    { root: "#closing-codex", arm: -120, swing: 18, rate: 2.4, length: 1.35, wave: M + .75, blinks: [M + 2.6], phase: 1.9 },
  ].map((mascot) => ({
    ...mascot,
    arms: $$(`${mascot.root} .clawd-wave-arm, ${mascot.root} .codex-wave-arm`),
    front: $(`${mascot.root} .clawd-wave-front, ${mascot.root} .codex-wave-front`),
    rest: $$(`${mascot.root} .clawd-wave-rest, ${mascot.root} .codex-wave-rest`),
    eyes: $(`${mascot.root} .clawd-eyes, ${mascot.root} .codex-eyes`),
    figure: $(`${mascot.root} .clawd-figure, ${mascot.root} .codex-figure`),
  }));
  function updateMascots(t) {
    for (const mascot of mascots) {
      const p = (t - mascot.wave) / mascot.length;
      const envelope = p < 0 || p > 1 ? 0 : smoothstep(clamp(p / .2)) * smoothstep(clamp((1 - p) / .2));
      const swing = mascot.swing * Math.sin((t - mascot.wave) * Math.PI * 2 * mascot.rate);
      // Each arm swings up from its shoulder and stays attached.
      const transform = `rotate(${(envelope * (mascot.arm + swing)).toFixed(2)}deg)`;
      for (const arm of mascot.arms) arm.style.transform = transform;
      const raised = envelope >= .3;
      mascot.front.setAttribute("opacity", raised ? "1" : "0");
      mascot.rest.forEach((rest) => rest.setAttribute("opacity", raised ? "0" : "1"));
      const blinking = mascot.blinks.some((at) => t >= at && t < at + .13);
      mascot.eyes.style.transform = blinking ? "scaleY(.12)" : "";
      const bob = Math.sin(t * Math.PI * 1.6 + mascot.phase) * 1.6;
      mascot.figure.style.transform = `translateY(${bob.toFixed(2)}px)`;
    }
  }

  // Claude Code draws at full size and scales with the window while it grows.
  const claudeScreen = $("#claude-screen");
  function updateWindow(t) {
    const p = windowEase(progress(t, T.claude.expand, T.claude.expand_duration));
    const width = INSTALL_WINDOW.width + (CLAUDE_WINDOW.width - INSTALL_WINDOW.width) * p;
    const height = INSTALL_WINDOW.height + (CLAUDE_WINDOW.height - INSTALL_WINDOW.height) * p;
    const scale = Math.min(1, width / CLAUDE_WINDOW.width, (height - BAR) / (CLAUDE_WINDOW.height - BAR));
    claudeScreen.style.transform = scale < 1 ? `scale(${scale.toFixed(4)})` : "";
  }

  // The board slides in from the right edge of the window as it opens.
  const boardPanel = $("#board");
  function updateBoard(t) {
    const p = 1 - (1 - progress(t, T.board.open, .45)) ** 3;
    boardPanel.style.opacity = p.toFixed(3);
    boardPanel.style.transform = p < 1 ? `translateX(${((1 - p) * 60).toFixed(2)}px)` : "";
  }

  function updateClosing(t) {
    const closing = typing.closing;
    const count = closing.times.filter((at) => at <= t).length;
    const text = closing.text.slice(0, count);
    $("#closing-with-fast").textContent = text.slice(0, 7);
    $("#closing-with-prefix").textContent = text.slice(7, 12);
    $("#closing-with-brand").textContent = text.slice(12);
    $("#closing-with-caret").style.display = t >= closing.times[0] - .05 && count < closing.text.length ? "inline-block" : "none";
    $(".closing-caret").style.opacity = Math.floor(t * 1.7) % 2 ? "1" : "0";
  }

  function update() {
    const t = tl.time();
    updateTitle(t);
    updateMascots(t);
    updateInstall(t);
    updateWindow(t);
    window.DELM_CLAUDE.render(t);
    updateBoard(t);
    updateClosing(t);
  }
  tl.eventCallback("onUpdate", update);
  window.__timelines = window.__timelines || {};
  window.__timelines.main = tl;
  window.DELM_SEEK = (time) => {
    tl.seek(time, false);
    update();
  };
  update();
})();
