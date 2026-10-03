(() => {
  const tl = gsap.timeline({ paused: true });
  const smooth = "power3.inOut";
  const reveal = "power3.out";
  const $$ = (s) => document.querySelector(s);
  const anchors = DELM_TIMING.anchors;
  const intro = DELM_TIMING.intro_seconds;
  const codexExtension = DELM_TIMING.codex_extension_seconds;
  const flow = DELM_WORKFLOW;
  const logoLines = $$(".install-logo").textContent.split("\n");
  // Carry the worker's exact initial content inside the moving terminal frame.
  const morphBody = $$("#worker-left .worker-body").cloneNode(true);
  morphBody.id = "worker-morph";
  morphBody.querySelectorAll("[id]").forEach(element => element.removeAttribute("id"));
  morphBody.querySelector(".worker-role").textContent = "Reading the task queue";
  morphBody.querySelector(".worker-file").style.opacity = "0";
  morphBody.querySelector(".feature-code").textContent = "";
  morphBody.querySelector(".worker-detail").textContent = "Choosing work from the shared queue.";
  $$("#installer").appendChild(morphBody);
  const morphCaption = document.createElement("span");
  morphCaption.id = "worker-morph-caption";
  morphCaption.className = "worker-name";
  morphCaption.textContent = "›_ Codex · Agent 1";
  $$("#installer > .window-bar").appendChild(morphCaption);

  function mapTime(time, reverse = false) {
    const input = reverse ? 1 : 0, output = reverse ? 0 : 1;
    for (let i = 1; i < anchors.length; i++) {
      const a = anchors[i - 1], b = anchors[i];
      if (time <= b[input]) return a[output] + (time - a[input]) * (b[output] - a[output]) / (b[input] - a[input]);
    }
    return anchors.at(-1)[output];
  }
  $$(".copy-command").textContent = DELM_FILM.installer;
  const show = (selector, start, duration = 0.5) =>
    tl.fromTo(
      selector,
      { opacity: 0 },
      { opacity: 1, duration, ease: reveal },
      start,
    );
  const hide = (selector, start, duration = 0.4) =>
    tl.to(selector, { opacity: 0, duration, ease: "power2.in" }, start);
  const enter = (selector, start, y = 24, duration = 0.65) =>
    tl.fromTo(
      selector,
      { opacity: 0, y },
      { opacity: 1, y: 0, duration, ease: reveal },
      start,
    );

  // The terminal is one rigid surface. Its projection is owned by Three.js.
  tl.fromTo("#installer-rig", { y: 36, opacity: 0 }, { y: 0, opacity: 1, duration: 1.05, ease: reveal }, 0);
  enter("#opening-title", 0.12, 28, 0.85);
  tl.set("#install-output", { opacity: 1 }, 3.5);
  tl.set("#install-step-1", { opacity: 1, y: 0 }, 3.6);
  tl.set("#install-step-2", { opacity: 1, y: 0 }, 3.96);
  tl.set("#install-step-3", { opacity: 1, y: 0 }, 4.12);
  show("#submitted-request", 12.15, 0.18);
  show("#codex-response", 12.25, 0.45);
  // Both workers stay visible. Shared files always pass through shared context.
  show("#collaboration", 13.25, 0.3);
  enter("#collab-title", 14, 24, 0.6);
  enter("#task-queue", 14.35, 24, 0.6);
  enter("#queue-integration", flow.integration_available, 6, 0.3);
  enter("#queue-checks", flow.checks_available, 6, 0.3);


  enter("#physics-feature", 16.1, 15, 0.5);
  enter("#audio-feature", 16.2, 15, 0.5);
  enter("#context-strip", 15.1, 18, 0.55);

  for (const [index, selector] of ["#context-core", "#context-audio", "#context-fix"].entries()) {
    const at = flow.shares[index].at + flow.publish_delay + flow.publish_duration;
    show(selector, at, .16);
    tl.fromTo(selector, { backgroundColor: "#edf1ff" }, { backgroundColor: "#f5f4f0", duration: .65, immediateRender: false }, at);
  }
  show("#worker-checks", flow.tests_pass, .18);
  for (const claim of flow.claims) {
    tl.fromTo(`#queue-${claim.task}`, { backgroundColor: "#edf1ff" }, { backgroundColor: "#f5f4f0", duration: .7, immediateRender: false }, claim.at);
  }
  // Keep the shared work visible until the completed game enters.
  tl.to("#collab-title", { y: -35, opacity: 0, duration: .5, ease: smooth }, 27.1);
  tl.to("#collaboration", { x: -180, scale: 0.93, opacity: 0, duration: 1.15, ease: smooth }, 27.2);

  // The real browser result takes over the frame as soon as play begins.
  show("#result-contributions", 27.6, 0.5);
  tl.fromTo("#browser-rig",
    { x: 100, y: 0, scale: 1, opacity: 0 },
    { x: 0, y: 0, scale: 1, opacity: 1, duration: 1.1, ease: smooth }, 27.1);
  tl.fromTo(
    "#pointer",
    { x: 710, y: 835, opacity: 0 },
    {
      x: 555,
      y: 745,
      opacity: 1,
      duration: 0.6,
      ease: smooth,
      immediateRender: false,
    },
    27.8,
  );
  tl.to("#pointer", { scale: 0.85, duration: 0.08 }, 28.59).to(
    "#pointer",
    { scale: 1, duration: 0.16 },
    28.67,
  );
  hide("#pointer", 28.9, 0.35);
  hide("#result-contributions", 33.4, .4);
  // Keep the completed game in its window; the contribution labels stay visible.
  hide("#browser-rig", 33.4, 0.6);
  show("#closing", 33.8, 0.4);
  enter("#closing-title .closing-line", 34.0, 24, .7);
  enter("#closing-command", 37.0, 14, .55);
  tl.to({}, { duration: 40 }, 0);
  const edits = tl.getChildren(false, true, false).map((child) => ({
    child, start: child.startTime(), end: child.startTime() + child.duration(),
  }));
  for (const { child, start, end } of edits) {
    child.duration(mapTime(end) - mapTime(start));
    child.startTime(mapTime(start));
  }

  // Retain one window across installation and launch, then pull it into Agent1.
  tl.set("#install-open", { opacity: 1 }, 6.55);
  tl.to("#opening-title", { y: -80, opacity: 0, duration: .72, ease: smooth }, 7.28);
  tl.to("#installer-rig", { left: 192, top: 112, width: 1536, height: 850, duration: 1.6, ease: smooth }, 7.28);
  tl.to("#install-screen", { y: -28, opacity: 0, duration: .3, ease: smooth }, 7.41);
  tl.fromTo("#codex-screen", { y: 24, opacity: 0 }, { y: 0, opacity: 1, duration: .35, ease: reveal }, 7.56);
  tl.to("#installer > .window-bar", { height: 62, duration: 1.6, ease: smooth }, 7.28);
  tl.to("#codex-screen", { top: 62, duration: 1.6, ease: smooth }, 7.28);
  tl.to("#installer-rig", { left: 90, top: 340, width: 500, height: 570, duration: 1.55, ease: smooth }, 17.25 + codexExtension);
  tl.to("#codex-screen", { opacity: 0, duration: .6, ease: "none" }, 17.55 + codexExtension);
  tl.to("#worker-morph", { opacity: 1, duration: .6, ease: "none" }, 17.55 + codexExtension);
  tl.to("#worker-morph", { top: 52, duration: 1.55, ease: smooth }, 17.25 + codexExtension);
  tl.set("#installer > .window-bar", { backgroundImage: "none", backgroundColor: "#f8f8f6" }, 17.25 + codexExtension);
  tl.to("#installer > .window-bar", { height: 52, duration: 1.55, ease: smooth }, 17.25 + codexExtension);
  tl.to("#installer .traffic, #terminal-caption, #installer .bar-spacer", { opacity: 0, duration: .45, ease: "none" }, 17.45 + codexExtension);
  tl.to("#worker-morph-caption", { opacity: 1, duration: .45, ease: "none" }, 17.45 + codexExtension);
  tl.to("#installer", { borderRadius: 16, borderColor: "#cfd5df", boxShadow: "0 18px 35px -28px #192a4930", duration: 1.55, ease: smooth }, 17.25 + codexExtension);
  // Swap only after both frame and contents exactly match the stationary worker.
  tl.set("#worker-left", { opacity: 1 }, 18.8 + codexExtension);
  tl.set("#installer-rig", { opacity: 0 }, 18.8 + codexExtension);
  tl.fromTo("#worker-right", { x: 180 }, { x: 0, duration: 1.25, ease: smooth }, 17.55 + codexExtension);
  tl.fromTo("#worker-right", { opacity: 0 }, { opacity: 1, duration: .6, ease: "none" }, 17.55 + codexExtension);

  for (const child of tl.getChildren(false, true, false)) child.startTime(child.startTime() + intro);
  tl.to("#cover-title, #install-copy", { y: -18, opacity: 0, duration: .32, ease: smooth }, intro - .32);

  // Each handoff has its own path, so simultaneous claims never overwrite one another.
  const routes = [];
  for (const event of flow.shares) {
    for (const importing of [false, true]) {
      const agent = importing ? 3 - event.author : event.author;
      const ax = agent === 1 ? 590 : 1330, bx = agent === 1 ? 670 : 1250;
      routes.push({start: event.at + (importing ? flow.import_delay : flow.publish_delay),
        span: importing ? flow.import_duration : flow.publish_duration,
        from: importing ? [bx, event.row] : [ax, 490], to: importing ? [ax, 490] : [bx, event.row],
        label: importing ? "Import" : "Share", corridor: agent === 1 ? 630 : 1290});
    }
  }
  for (const event of [{arrive: flow.integration_available, agent: 1, row: 824}, {arrive: flow.checks_available, agent: 2, row: 879}]) {
    routes.push({start: event.arrive - .45, span: .45,
      from: [event.agent === 1 ? 590 : 1330, 445], to: [event.agent === 1 ? 670 : 1250, event.row],
      label: "Add task", corridor: event.agent === 1 ? 630 : 1290});
  }
  for (const event of flow.claims) {
    routes.push({start: event.at, span: flow.claim_duration,
      from: [event.agent === 1 ? 670 : 1250, event.row], to: [event.agent === 1 ? 590 : 1330, 445],
      label: "Claim", corridor: event.agent === 1 ? 630 : 1290});
  }
  const routeTemplate = $$("#transfer-route");
  for (const event of routes) {
    const element = routeTemplate.cloneNode(true);
    element.removeAttribute("id"); element.classList.add("transfer-route");
    event.path = element.querySelector("path");
    event.token = element.querySelector("#transfer-token");
    event.labelElement = element.querySelector("#transfer-label");
    event.labelElement.classList.add("transfer-label");
    if (event.label === "Add task") {
      event.labelElement.querySelector("rect").setAttribute("x", "-43");
      event.labelElement.querySelector("rect").setAttribute("width", "86");
    }
    element.querySelectorAll("[id]").forEach(node => node.removeAttribute("id"));
    event.element = element;
    routeTemplate.parentElement.appendChild(element);
  }
  routeTemplate.remove();

  const progress = (time, start, duration) =>
    Math.max(0, Math.min(1, (time - start) / duration));
  function write(el, text, p) {
    el.textContent = text.slice(0, Math.floor(text.length * p));
  }
  function update() {
    const filmTime = tl.time() - intro;
    const t = mapTime(filmTime, true);
    const collaborationTime = filmTime - codexExtension;
    $$(".install-logo").textContent = logoLines.slice(0, Math.max(0, Math.min(logoLines.length, Math.floor((t - 3.5) / .018) + 1))).join("\n");
    $$("#terminal-caption").textContent = filmTime < 7.66 ? "Terminal" : "Codex · neon-rush";
    window.DELM_TERMINAL_CAMERA?.draw(filmTime);
    write($$("#open-codex-typed"), "codex", progress(filmTime, 6.58, .38));
    const installerTyping = DELM_TYPING.find(field => field.name === "installer");
    $$("#install-typed").textContent = installerTyping.text.slice(0, installerTyping.times.filter(at => at <= t).length);
    $$("#install-caret").style.opacity =
      t < 3.35 && Math.floor(t * 2.3) % 2 === 0 ? "1" : "0";
    const submitted = t >= 12.15;
    $$("#skill-typed").textContent = submitted ? "" : DELM_TYPING[0].text.slice(0, DELM_TYPING[0].times.filter(at => at <= t).length);
    $$("#prompt-typed").textContent = submitted ? "" : DELM_TYPING[1].text.slice(0, DELM_TYPING[1].times.filter(at => at <= t).length);
    $$("#submitted-prompt").textContent = DELM_FILM.prompt;
    $$("#prompt-placeholder").style.opacity = t < 7.67 || submitted ? "1" : "0";
    $$("#prompt-caret").style.opacity =
      t >= 7.0 && Math.floor(t * 2.3) % 2 === 0 ? "1" : "0";

    const publication = event => event.at + flow.publish_delay + flow.publish_duration;
    const imported = event => event.at + flow.import_delay + flow.import_duration;
    const core = flow.shares[0], audio = flow.shares[1], fix = flow.shares[2];
    const claims = Object.fromEntries(flow.claims.map(event => [event.task, event]));
    const claimed = task => claims[task].at + flow.claim_duration;
    const done = {physics: claims.integration.at - .05, audio: publication(audio) + .3,
      integration: flow.tests_pass, checks: flow.tests_pass};
    for (const event of flow.claims) {
      const element = $$(`#queue-${event.task}-owner`);
      element.textContent = t < event.at ? "Available" : t < done[event.task] ? `Claimed by Agent ${event.agent}` : `✓ Agent ${event.agent}`;
      element.style.color = element.textContent.startsWith("Claimed") ? "#315ee8" : "#747e8d";
    }
    $$("#context-core-file").textContent = "core.js";
    $$("#context-core-text").textContent = "Jump controls ready";

    const leftReady = t >= claimed("physics"), rightReady = t >= claimed("audio");
    for (const [side, ready] of [["left", leftReady], ["right", rightReady]]) {
      $$(`#worker-${side} .worker-file`).style.opacity = ready ? "1" : "0";
    }
    const physicsCode = "this.vy = -JUMP;\nemit('jump');";
    const audioCode = "add('kick', beat, .72);\nadd('clap', beat + 1, .4);";
    if (!leftReady) $$("#worker-left-code").textContent = "";
    else if (t < 18.05) write($$("#worker-left-code"), physicsCode, progress(t, 16.1, 1.0));
    else if (t < claimed("integration")) write($$("#worker-left-code"), "spike(6); spike(10);\ngap(18, 100); platform(22, 148, 44);", progress(t, 18.05, .65));
    else if (t < imported(fix)) write($$("#worker-left-code"), "sim.advance(dt, input);\naudio.update(sim.elapsed);", progress(t, claimed("integration"), .9));
    else $$("#worker-left-code").textContent = t < flow.tests_pass ? "node --test tests/*.test.js\nRunning gameplay + audio checks…" : "25 passed · 0 failed";
    $$("#worker-left-action").textContent = t < imported(fix) ? "Edited" : "Ran";
    $$("#worker-left-file").textContent = t >= imported(fix) ? "game checks" : t < claimed("integration") ? "js/core.js" : "js/game.js";
    $$("#worker-left .worker-role").textContent = !leftReady ? "Reading the task queue" : t < claimed("integration") ? "#1 Physics + level" : "#3 Integrate the game";
    if (!rightReady) $$("#worker-right-code").textContent = "";
    else if (t < 17.1) write($$("#worker-right-code"), audioCode, progress(t, 16.05, .9));
    else if (t < 18.05) write($$("#worker-right-code"), "add('bass', beat, 1.45, root);\nadd('bass', beat + 2, 1.3, root);", progress(t, 17.1, .7));
    else if (t < claimed("checks")) write($$("#worker-right-code"), "const arp = [0,2,1,3,2,1,3,2];\nconst rate = finalRun ? .25 : .5;", progress(t, 18.05, .65));
    else if (t < flow.bug_found) $$("#worker-right-code").textContent = "Checking pause + restart…";
    else if (t < flow.fix_started) $$("#worker-right-code").textContent = "Best progress was not saved on pause.";
    else write($$("#worker-right-code"), "saveBest(); clearInput();\naudio.pause(); show('paused');", progress(t, flow.fix_started, .55));
    $$("#worker-right-action").textContent = t >= claimed("checks") && t < flow.fix_started ? "Ran" : "Edited";
    $$("#worker-right-file").textContent = t < claimed("checks") ? "js/audio.js" : t < flow.fix_started ? "controller checks" : "js/game.js";
    $$("#worker-right .worker-role").textContent = !rightReady ? "Reading the task queue" : t < claimed("checks") ? "#2 Soundtrack + effects" : "#4 Controller checks";
    $$("#worker-left-detail").textContent = !leftReady ? "Choosing work from the shared queue." : t < 17.1 ? "Adding jump controls." : t < publication(core) ? "Sharing working jump controls." : t < imported(audio) ? "Building obstacle sequences." : t < claimed("integration") ? "Imported audio.js. Ready to integrate." : t < imported(fix) ? "Imported audio.js. Connecting playback." : t < flow.tests_pass ? "Imported fix. Checking the complete game." : "Complete game. All 25 tests pass.";
    $$("#worker-right-detail").textContent = !rightReady ? "Choosing work from the shared queue." : t < 17.1 ? "Building the drum pattern." : t < 18.05 ? "Adding the bassline." : t < imported(core) ? "Composing arpeggios and melody." : t < publication(audio) ? "Imported core.js. Testing audio." : t < claimed("checks") ? "Published soundtrack + audio tests." : t < flow.bug_found ? "Checking pause, restart, and saved progress." : t < flow.fix_started ? "Found a pause-save bug." : t < publication(fix) ? "Fixing progress save and sharing tests." : t < imported(fix) ? "Correction published to shared context." : t < flow.tests_pass ? "Agent 1 is checking the integrated game." : "Correction verified in the complete game.";
    $$("#physics-feature .preview-label").textContent = collaborationTime < 23.14 ? "Movement" : collaborationTime < 24.5 ? "Jump controls" : "Movement + obstacles";
    $$("#audio-feature .preview-label").textContent = collaborationTime < 23 ? "Drums · 140 BPM" : collaborationTime < 24.5 ? "Drums + bass" : "Soundtrack · 140 BPM";
    window.DELM_FEATURES.draw(14 + Math.max(0, collaborationTime - 18));
    for (const event of routes) {
      const route = event.element;
      route.style.opacity = "0";
      if (t < event.start || t > event.start + event.span) continue;
      const [fromX, fromY] = event.from, [toX, toY] = event.to;
      const routeX = event.corridor, sx = Math.sign(routeX - fromX), dy = Math.sign(toY - fromY), tx = Math.sign(toX - routeX), r = 10;
      const path = event.path;
      path.setAttribute("d", `M${fromX} ${fromY}H${routeX-sx*r}Q${routeX} ${fromY} ${routeX} ${fromY+dy*r}V${toY-dy*r}Q${routeX} ${toY} ${routeX+tx*r} ${toY}H${toX}`);
      const p = progress(t, event.start, event.span), eased = p*p*(3-2*p);
      const point = path.getPointAtLength(eased * path.getTotalLength());
      event.token.setAttribute("transform", `translate(${point.x} ${point.y})`);
      event.labelElement.setAttribute("transform", `translate(${routeX} ${Math.min(fromY,toY)-25})`);
      event.labelElement.querySelector("text").textContent = event.label;
      route.style.opacity = String(Math.min(1, p*9, (1-p)*9));
    }
    const closingTyping = DELM_TYPING.find(field => field.name === "closing");
    const closingCount = closingTyping.times.filter(at => at <= t).length;
    const closingText = closingTyping.text.slice(0, closingCount);
    $$("#closing-with-fast").textContent = closingText.slice(0, 7);
    $$("#closing-with-prefix").textContent = closingText.slice(7, 12);
    $$("#closing-with-brand").textContent = closingText.slice(12);
    $$("#closing-with-caret").style.display = t >= 34.85 && closingCount < closingTyping.text.length ? "inline-block" : "none";
    $$(".closing-caret").style.opacity = Math.floor(t * 1.7) % 2 ? "1" : "0";
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
