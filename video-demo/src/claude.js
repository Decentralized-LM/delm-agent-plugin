// Claude Code, the DeLM board, and the two agents, drawn on a strict character grid.
// Board colors and layout follow a native 120-column capture of the board pane.
(() => {
  const COLUMNS = 129, LEFT = 71, PANE_ROWS = 26, LINE = 28, PANE_COLUMNS = 31;
  const film = DELM_FILM, T = DELM_TIMING, board = T.board, claude = T.claude;
  const typing = Object.fromEntries(DELM_TYPING.map((field) => [field.name, field]));
  const $ = (selector) => document.querySelector(selector);
  const rows = $("#claude-rows"), boardRows = $("#board-rows"), routeLayer = $("#routes");
  const escape = (text) => String(text).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  const clamp = (value) => Math.max(0, Math.min(1, value));

  const typed = (name, t) => {
    const field = typing[name];
    return field.text.slice(0, field.times.filter((at) => at <= t).length);
  };

  // Claude Code's terminal mascot, decoded from the captured block characters.
  const LOGO = [
    "   #############    ",
    "   ##o#######o##    ",
    " #################  ",
    "   #############    ",
    "   # #       # #    ",
  ];
  const logo = LOGO.flatMap((line, y) => [...line].map((cell, x) => cell === " " ? "" :
    `<rect x="${x}" y="${y}" width="1.02" height="1.02" fill="${cell === "o" ? "#000" : "#d77757"}"/>`)).join("");
  const LOGO_SVG = `<svg class="claude-logo" viewBox="0 0 20 6" preserveAspectRatio="none" shape-rendering="crispEdges">${logo}</svg>`;

  function wrap(text, width) {
    const lines = [];
    let line = "";
    for (const word of text.split(" ")) {
      if (line && line.length + 1 + word.length > width) {
        lines.push(line);
        line = word;
      } else line = line ? `${line} ${word}` : word;
    }
    if (line) lines.push(line);
    return lines;
  }

  // A segment is placed by column, so fallback symbol glyphs never shift the grid.
  const seg = (col, text, cls = "", style = "") =>
    `<span class="${cls}" style="left:${col}ch;${style}">${escape(text)}</span>`;
  const fill = (col, width, color, cls = "fill") =>
    `<span class="${cls}" style="left:${col}ch;width:${width}ch;background:${color}"></span>`;
  const row = (index, content, style = "") =>
    `<div class="row" style="top:${index * LINE}px;${style}">${content}</div>`;
  function flash(t, since, col, width) {
    const age = t - since;
    if (!(age >= 0 && age < 1.3)) return "";
    const strength = (1 - age / 1.3) ** 2;
    return fill(col - .5, width + 1, `rgba(107, 143, 255, ${(.26 * strength).toFixed(3)})`);
  }
  const fadeIn = (t, since) => `opacity:${clamp((t - since) / .22).toFixed(3)}`;

  // Routes move work between the agents and the board; their arrivals change the board.
  const routes = T.routes.map((route) => ({ ...route, end: route.at + T.route_duration[route.kind] }));
  const arrival = (kind, key, value) => routes.find((route) => route.kind === kind && route[key] === value).end;
  const claimedAt = (id) => arrival("claim", "task", id);
  const doneAt = (id) => board.done[id];
  const owner = { 1: 1, 2: 2, 3: 1, 4: 2 };
  const shares = board.shares.map((share, index) => ({
    ...share, index, at: arrival("share", "share", index), import: arrival("import", "share", index),
  }));
  const firstClaim = Math.min(claimedAt(1), claimedAt(2));
  const lastDone = Math.max(...Object.values(board.done));
  const taskTitle = (id) => film.tasks.find((task) => task.id === id).title;

  function agentState(agent, t) {
    let state = { label: "Starting", since: board.open };
    for (const id of agent === 1 ? [1, 3] : [2, 4]) {
      if (t >= claimedAt(id)) state = { task: id, since: claimedAt(id) };
      if (t >= doneAt(id)) state = { label: "Ready", since: doneAt(id), last: id };
    }
    return state;
  }

  function boardState(t) {
    const phase = t < firstClaim ? ["Starting agents", board.open]
      : t < lastDone ? ["Working in parallel", firstClaim]
      : t < board.delivered ? ["Finishing", lastDone]
      : ["Changes applied", board.delivered];
    const tasks = [1, 2, 3, 4].filter(() => t >= board.tasks).map((id) =>
      t < claimedAt(id) ? { id, status: "Available", since: board.tasks }
        : t < doneAt(id) ? { id, status: `Claimed · Agent ${owner[id]}`, since: claimedAt(id) }
        : { id, status: `Done · Agent ${owner[id]}`, since: doneAt(id) });
    const shared = shares.filter((share) => t >= share.at).reverse().slice(0, 2);
    return { phase, agents: [agentState(1, t), agentState(2, t)], tasks, shared };
  }

  // Board row positions, shared by the board and the routes that point at it.
  function boardLayout(t) {
    const { tasks, shared } = boardState(t);
    const taskRow = Object.fromEntries(tasks.map((task, index) => [task.id, 8 + index]));
    const sharedHeader = 8 + Math.max(1, tasks.length) + 1;
    const sharedRow = Object.fromEntries(shared.map((share, index) => [share.index, sharedHeader + 1 + index * 5]));
    return { taskRow, sharedHeader, sharedRow };
  }

  function claudeRows(t) {
    const html = [];
    const boardOpen = t >= board.open;
    const width = boardOpen ? LEFT - 2 : COLUMNS - 2;
    html.push(`<div class="row" style="top:${LINE}px;height:${LINE * 3}px">${LOGO_SVG}</div>`);
    html.push(row(1, seg(11, "Claude Code", "bright bold") + seg(23, film.claudeVersion, "dim")));
    html.push(row(2, seg(11, film.claudeModel, "dim")));
    html.push(row(3, seg(11, film.project, "dim")));

    let line = 5;
    if (t >= claude.submit) {
      for (const [index, text] of wrap(`${film.command} ${film.prompt}`, width - 2).entries()) {
        html.push(row(line++, fill(0, text.length + 3, "#373737") +
          (index ? "" : seg(0, "❯", "dim")) + seg(2, text, "bright")));
      }
      line++;
    }
    for (const [at, message] of [[claude.reply, film.reply], [board.final_reply, film.finalReply]]) {
      if (t < at) continue;
      const style = fadeIn(t, at);
      for (const [index, text] of wrap(message, width - 2).entries()) {
        html.push(row(line++, (index ? "" : seg(0, "⏺", "bright")) + seg(2, text), style));
      }
      line++;
    }

    const effort = "◐ medium · /effort";
    const effortEnd = boardOpen ? LEFT - 2 : COLUMNS - 1;
    html.push(row(25, seg(effortEnd - effort.length, "◐", "dim") + seg(effortEnd - effort.length + 2, effort.slice(2), "dim")));
    html.push(`<div class="rule" style="top:${26 * LINE + LINE / 2}px"></div>`);
    html.push(`<div class="rule" style="top:${28 * LINE + LINE / 2}px"></div>`);

    const submitted = t >= claude.submit;
    const slash = typed("slash", t);
    const completed = t >= claude.complete;
    const input = submitted ? "" : completed ? `${film.command} ${typed("prompt", t)}` : slash;
    const caretOn = t < claude.slash[0] || submitted ? Math.floor(t * 1.9) % 2 === 0 : true;
    html.push(row(27, seg(0, "❯") + seg(2, input) +
      (caretOn ? `<span class="caret" style="left:${2 + input.length}ch"></span>` : "")));

    if (slash.length >= 2 && !completed) {
      const description = film.commandDescription, room = COLUMNS - 32;
      html.push(row(29, seg(2, film.command, "suggest") +
        seg(30, description.length > room ? `${description.slice(0, room - 1)}…` : description, "suggest")));
    } else {
      html.push(row(29, seg(2, t >= claude.reply ? "? for shortcuts · ← for agents" : "? for shortcuts", "dim")));
    }
    rows.innerHTML = html.join("");
  }

  function boardRowsAt(t) {
    const html = [];
    const { phase, agents, tasks, shared } = boardState(t);
    const { sharedHeader } = boardLayout(t);
    const text = 2;
    for (let index = 0; index < PANE_ROWS; index++) html.push(row(index, seg(0, "│", "divider")));
    html.push(row(0, seg(COLUMNS - LEFT - 2, "✕", "dim")));
    html.push(row(1, flash(t, phase[1], text + 6, phase[0].length) +
      seg(text, "DeLM", "blue bold") + seg(text + 6, phase[0], "dim")));

    html.push(row(3, seg(text, "AGENTS", "dim")));
    agents.forEach((agent, index) => {
      const label = agent.task ? `#${agent.task} ${taskTitle(agent.task)}` : agent.label;
      const content = agent.task
        ? seg(text, `${index + 1}  ${label}`, "bright bold") + seg(text + 3 + label.length, " · Working", "blue")
        : seg(text, `${index + 1}`, "bright bold") + seg(text + 3, label, "dim");
      html.push(row(4 + index, flash(t, agent.since, text + 3, label.length + (agent.task ? 10 : 0)) + content));
    });

    html.push(row(7, seg(text, "TASK QUEUE", "dim")));
    if (!tasks.length) html.push(row(8, seg(text, "No tasks shared yet", "dim")));
    tasks.forEach((item, index) => {
      html.push(row(8 + index, flash(t, item.since, text + 26, item.status.length) +
        seg(text, `#${item.id}`, "dim") + seg(text + 4, taskTitle(item.id)) +
        seg(text + 26, item.status, item.status === "Available" ? "dim" : ""), fadeIn(t, board.tasks)));
    });
    html.push(row(sharedHeader, seg(text, "SHARED CONTEXT", "dim")));
    if (!shared.length) html.push(row(sharedHeader + 1, seg(text, "No findings or code shared yet", "dim")));
    shared.forEach((entry, index) => {
      const top = sharedHeader + 1 + index * 5, style = fadeIn(t, entry.at);
      const age = Math.max(1, Math.floor(t - entry.at));
      html.push(row(top, flash(t, entry.at, text, entry.title.length) + seg(text, entry.title, "bright bold"), style));
      html.push(row(top + 1, seg(text, `Agent ${entry.author} published · ${age}s ago`, "dim"), style));
      html.push(row(top + 2, seg(text, entry.body), style));
      if (t >= entry.import) {
        const imported = `Agent ${3 - entry.author} imported this contribution.`;
        html.push(row(top + 3, flash(t, entry.import, text, imported.length) + seg(text, imported, "blue"), fadeIn(t, entry.import)));
      }
    });
    html.push(row(24, seg(text, "d", "key") + seg(text + 1, ": Details") + seg(text + 13, "h", "key") + seg(text + 14, ": Hide")));
    boardRows.innerHTML = html.join("");
  }

  // Agent panes: the current task, the latest tool call, its code, and a live preview.
  const panes = [1, 2].map((agent) => {
    const element = $(`#agent-${agent}`);
    const label = document.createElement("div");
    label.className = "preview-label";
    element.querySelector(".agent-preview").prepend(label);
    return { agent, rows: element.querySelector(".agent-rows"), label };
  });
  const PREVIEW_LABELS = {
    1: [[0, "Movement"], [25.9, "Jump controls"], [27.3, "Movement + obstacles"]],
    2: [[0, "Drums · 140 BPM"], [25.2, "Drums + bass"], [27.7, "Soundtrack · 140 BPM"]],
  };

  function agentPane({ agent, rows: target, label }, t) {
    const view = {};
    let code = null, tool = null;
    for (const beat of T.agents[agent]) {
      if (t < beat.at) break;
      if (beat.detail) Object.assign(view, { detail: beat.detail, since: beat.at });
      if (beat.tool) tool = beat;
      if (beat.code) code = beat;
    }
    const state = agentState(agent, t);
    const status = state.task ? ["Working", "blue"] : [state.label, "dim"];
    const html = [row(0, seg(0, `Agent ${agent}`, "bright bold") + seg(PANE_COLUMNS - status[0].length, status[0], status[1]))];
    const task = state.task || state.last;
    html.push(row(1, task ? seg(0, `#${task} ${taskTitle(task)}`, state.task ? "" : "dim") : seg(0, "Reading the task queue", "dim")));
    if (tool) {
      html.push(row(2, flash(t, tool.at, 0, tool.tool.length + tool.file.length + 4) +
        seg(0, "⏺", "ok") + seg(2, tool.tool, "bright bold") + seg(2 + tool.tool.length, `(${tool.file})`)));
    }
    if (code) {
      const progress = code.type ? clamp((t - code.at) / code.type) : 1;
      const total = code.code.join("").length;
      let shown = Math.floor(total * progress);
      const tone = code.tone === "ok" ? "ok" : code.tone === "bad" ? "bad" : "";
      code.code.forEach((text, index) => {
        const visible = text.slice(0, Math.max(0, shown));
        shown -= text.length;
        const background = tone === "bad" ? "rgba(255, 107, 107, .16)" : tone === "ok" ? "rgba(78, 186, 101, .16)" : "rgba(78, 186, 101, .13)";
        html.push(row(3 + index, (index ? "" : seg(2, "⎿", "dim")) +
          (visible ? fill(5, visible.length + 1, background) : "") + seg(5, visible, tone)));
      });
    }
    if (view.detail) html.push(row(5, flash(t, view.since, 0, view.detail.length) + seg(0, view.detail, "dim")));
    target.innerHTML = html.join("");
    label.textContent = PREVIEW_LABELS[agent].filter(([at]) => t >= at).at(-1)[1];
  }

  // Route geometry in Claude Code screen pixels. Each agent has its own track.
  const rowCenter = (index) => 18 + index * LINE + LINE / 2;
  const TRACK = { 1: { x: 922, y: 305, pane: 251 }, 2: { x: 934, y: 319, pane: 697 } };
  const PANE_TOP = 326, BOARD_TEXT = 966;
  routeLayer.innerHTML = `<defs><radialGradient id="route-glow"><stop offset="0" stop-color="#6b8fff" stop-opacity=".55"/><stop offset="1" stop-color="#6b8fff" stop-opacity="0"/></radialGradient></defs><g id="route-items"></g>`;
  const routeItems = routeLayer.querySelector("#route-items");

  function routePoints(route) {
    const track = TRACK[route.agent];
    const toBoard = route.kind === "share";
    const layout = boardLayout(toBoard ? route.end : route.at);
    const boardRow = route.task ? layout.taskRow[route.task] : layout.sharedRow[route.share];
    const y = rowCenter(boardRow);
    const points = [[track.pane, PANE_TOP], [track.pane, track.y], [track.x, track.y], [track.x, y], [BOARD_TEXT, y]];
    return toBoard ? points : points.reverse();
  }

  function pointAt(points, distance) {
    for (let index = 1; index < points.length; index++) {
      const [ax, ay] = points[index - 1], [bx, by] = points[index];
      const length = Math.hypot(bx - ax, by - ay);
      if (distance <= length) return [ax + (bx - ax) * distance / length, ay + (by - ay) * distance / length];
      distance -= length;
    }
    return points.at(-1);
  }

  function routesAt(t) {
    const html = [];
    for (const route of routes) {
      const linger = .4;
      if (t < route.at || t > route.end + linger) continue;
      const points = routePoints(route);
      const length = points.slice(1).reduce((sum, [x, y], index) => sum + Math.hypot(x - points[index][0], y - points[index][1]), 0);
      const p = clamp((t - route.at) / (route.end - route.at)), eased = p * p * (3 - 2 * p);
      const opacity = t > route.end ? 1 - (t - route.end) / linger : Math.min(1, p * 8);
      const [x, y] = pointAt(points, eased * length);
      const track = TRACK[route.agent], word = route.label;
      const labelX = (track.pane + track.x) / 2, labelWidth = word.length * 9.2 + 28;
      html.push(`<g opacity="${opacity.toFixed(3)}">` +
        `<polyline points="${points.map((point) => point.join(",")).join(" ")}" fill="none" stroke="#6b8fff" stroke-width="2.4" stroke-linejoin="round" stroke-linecap="round" stroke-dasharray="${length.toFixed(1)}" stroke-dashoffset="${((1 - eased) * length).toFixed(1)}"/>` +
        `<g class="route-label" transform="translate(${labelX} ${track.y})"><rect x="${-labelWidth / 2}" y="-14" width="${labelWidth}" height="28" rx="14"/><text text-anchor="middle" dominant-baseline="central">${word}</text></g>` +
        `<circle cx="${x.toFixed(1)}" cy="${y.toFixed(1)}" r="18" fill="url(#route-glow)"/>` +
        `<g transform="translate(${x.toFixed(1)} ${y.toFixed(1)})"><path d="M-8-11H3L8-6V11H-8Z" fill="#eef2ff" stroke="#6b8fff" stroke-width="1.6"/><path d="M3-11V-6H8M-4 0H4M-4 5H4" fill="none" stroke="#6b8fff" stroke-width="1.4"/></g>` +
        `</g>`);
    }
    routeItems.innerHTML = html.join("");
  }

  window.DELM_CLAUDE = Object.freeze({
    render(t) {
      claudeRows(t);
      boardRowsAt(t);
      for (const pane of panes) agentPane(pane, t);
      routesAt(t);
      window.DELM_FEATURES.draw(14 + (t - T.agents.features));
    },
  });
})();
