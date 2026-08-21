const PIP_XY = {
  1: [[50, 50]],
  2: [[28, 28], [72, 72]],
  3: [[28, 28], [50, 50], [72, 72]],
  4: [[28, 28], [72, 28], [28, 72], [72, 72]],
  5: [[28, 28], [72, 28], [50, 50], [28, 72], [72, 72]],
  6: [[28, 28], [72, 28], [28, 50], [72, 50], [28, 72], [72, 72]],
};

function dieFaceSvg(face) {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 100 100");
  svg.setAttribute("width", "100");
  svg.setAttribute("height", "100");
  svg.setAttribute("preserveAspectRatio", "xMidYMid meet");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("focusable", "false");
  for (const [x, y] of PIP_XY[face] || []) {
    const c = document.createElementNS("http://www.w3.org/2000/svg", "circle");
    c.setAttribute("cx", String(x));
    c.setAttribute("cy", String(y));
    c.setAttribute("r", "11");
    svg.appendChild(c);
  }
  return svg;
}

const state = {
  ws: null,
  playerId: null,
  playerName: localStorage.getItem("bones-name") || "",
  seatKey: null,
  game: null,
  boundCode: null,
  selected: new Set(),
  reconnecting: false,
};

const $ = (id) => document.getElementById(id);

function makeUuid() {
  if (globalThis.crypto?.randomUUID) {
    try {
      return crypto.randomUUID();
    } catch {
      /* insecure http origins */
    }
  }
  const bytes = new Uint8Array(16);
  if (globalThis.crypto?.getRandomValues) crypto.getRandomValues(bytes);
  else for (let i = 0; i < 16; i++) bytes[i] = (Math.random() * 256) | 0;
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

function seatKey() {
  if (state.seatKey) return state.seatKey;
  let key = localStorage.getItem("bones-seat");
  if (!key) {
    key = makeUuid();
    localStorage.setItem("bones-seat", key);
  }
  state.seatKey = key;
  return key;
}

function rotateSeatKey() {
  const key = makeUuid();
  localStorage.setItem("bones-seat", key);
  state.seatKey = key;
  return key;
}

function toast(msg) {
  const el = $("toast");
  el.textContent = msg;
  el.classList.remove("hidden");
  clearTimeout(toast._t);
  toast._t = setTimeout(() => el.classList.add("hidden"), 3200);
}

async function copyText(text) {
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      /* http LAN */
    }
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.cssText = "position:fixed;left:-9999px;top:0";
  document.body.appendChild(ta);
  ta.focus();
  ta.select();
  ta.setSelectionRange(0, text.length);
  let ok = false;
  try {
    ok = document.execCommand("copy");
  } catch {
    ok = false;
  }
  ta.remove();
  return ok;
}

/** Always `/g/{CODE}` — never origin alone. */
function inviteUrlFor(code) {
  if (!code) return "";
  return `${location.origin}/g/${encodeURIComponent(String(code).toUpperCase())}`;
}

function applyInvite(code, invitePath) {
  if (!code) return;
  const path = invitePath && invitePath.startsWith("/g/")
    ? invitePath
    : `/g/${String(code).toUpperCase()}`;
  const url = `${location.origin}${path}`;
  history.replaceState(null, "", path);
  const input = $("invite-url");
  if (input) input.value = url;
  const room = $("room-code");
  if (room) room.textContent = String(code).toUpperCase();
}

function pathCode() {
  const fromPath = location.pathname.match(/^\/g\/([A-Za-z0-9]+)/i);
  if (fromPath) return fromPath[1].toUpperCase();
  const fromQuery = new URLSearchParams(location.search).get("code");
  return fromQuery ? fromQuery.trim().toUpperCase() : "";
}

function storedRoomCode() {
  return (localStorage.getItem("bones-room") || "").trim().toUpperCase();
}

/** Invite URL wins; otherwise the last table this phone was sitting at. */
function lastRoomCode() {
  return pathCode() || storedRoomCode();
}

function rememberRoom(code) {
  if (!code) return;
  localStorage.setItem("bones-room", String(code).toUpperCase());
}

function forgetRoom() {
  localStorage.removeItem("bones-room");
}

function socketLooksAlive() {
  return (
    !!state.ws &&
    state.ws.readyState === WebSocket.OPEN &&
    !!state.playerId &&
    Date.now() - (state.lastServerAt || 0) < 35_000
  );
}

function dropSocket() {
  const ws = state.ws;
  if (!ws) return;
  state.ws = null;
  connecting = null;
  state.playerId = null;
  try {
    ws.close();
  } catch {
    /* already closed */
  }
}

function wsUrl() {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  return `${proto}://${location.host}/ws`;
}

let connecting = null;

function connect() {
  if (state.ws?.readyState === WebSocket.OPEN && state.playerId) {
    return Promise.resolve(state.ws);
  }
  if (connecting) return connecting;
  connecting = new Promise((resolve, reject) => {
    const ws = new WebSocket(wsUrl());
    state.ws = ws;
    state.playerId = null;
    let settled = false;

    const timer = setTimeout(() => {
      fail(new Error("Server did not welcome the connection"));
      try {
        ws.close();
      } catch {
        /* already closed */
      }
    }, 8_000);

    function succeed(value) {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(value);
    }

    function fail(err) {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(err);
    }

    ws.addEventListener("open", () => {
      startHeartbeat(ws);
    });
    ws.addEventListener("error", () => fail(new Error("Could not connect")));
    ws.addEventListener("message", (ev) => {
      let msg;
      try {
        msg = JSON.parse(ev.data);
      } catch (e) {
        console.error(e);
        return;
      }
      onServer(msg);
      if (msg.type === "welcome") succeed(ws);
    });
    ws.addEventListener("close", () => {
      if (!settled) {
        fail(new Error("Disconnected"));
        return;
      }
      if (state.ws !== ws) return;
      if (lastRoomCode()) scheduleReconnect("Connection lost — reclaiming your seat…");
      else toast("Disconnected — refresh to rejoin");
    });
  }).finally(() => {
    connecting = null;
  });
  return connecting;
}

const PING_STALE_MS = 45_000;

function startHeartbeat(ws) {
  state.lastServerAt = Date.now();
  const timer = setInterval(() => {
    if (state.ws !== ws || ws.readyState !== WebSocket.OPEN) {
      clearInterval(timer);
      return;
    }
    if (Date.now() - state.lastServerAt > PING_STALE_MS) {
      clearInterval(timer);
      ws.close();
    }
  }, 5_000);
}

async function joinLastRoom() {
  const code = lastRoomCode();
  if (!code) return false;
  const name = (state.playerName || localStorage.getItem("bones-name") || "").trim();
  if (!name) return false;
  state.playerName = name;
  applyInvite(code);
  rememberRoom(code);
  if (!socketLooksAlive()) {
    dropSocket();
    await connect();
  }
  send({ type: "join_game", code, name, seat_key: seatKey() });
  return true;
}

function scheduleReconnect(message) {
  if (state.reconnecting) return;
  if (!lastRoomCode()) return;
  state.reconnecting = true;
  toast(message || "Rejoining your table…");
  const attempt = async (n) => {
    try {
      if (!state.reconnecting) return;
      const ok = await joinLastRoom();
      if (!ok) {
        state.reconnecting = false;
        return;
      }
      state.reconnecting = false;
    } catch {
      if (!state.reconnecting) return;
      setTimeout(() => attempt(n + 1), Math.min(1000 * 2 ** n, 15000));
    }
  };
  setTimeout(() => attempt(0), 400);
}

function resumeIfNeeded() {
  const code = lastRoomCode();
  if (!code) return;
  if (!(state.playerName || localStorage.getItem("bones-name") || "").trim()) return;
  if (socketLooksAlive() && state.boundCode === code && state.game?.code === code) return;
  scheduleReconnect("Rejoining your table…");
}

function send(msg) {
  if (state.ws?.readyState === WebSocket.OPEN) {
    state.ws.send(JSON.stringify(msg));
  }
}

function onServer(msg) {
  state.lastServerAt = Date.now();
  switch (msg.type) {
    case "welcome":
      state.playerId = msg.player_id;
      break;
    case "ping":
      send({ type: "pong" });
      break;
    case "error":
      if (msg.message === "Game not found") {
        const wasAtTable = Boolean(state.game?.code || state.reconnecting || storedRoomCode());
        goHome(wasAtTable ? "That table is gone" : msg.message);
        break;
      }
      showHomeError(msg.message);
      toast(msg.message);
      break;
    case "game_created":
    case "joined":
      state.boundCode = msg.code;
      state.reconnecting = false;
      applyInvite(msg.code, msg.invite_path);
      rememberRoom(msg.code);
      break;
    case "state":
      if (!state.boundCode || msg.code !== state.boundCode) break;
      {
        const facesKey = (msg.dice || []).join(",");
        const kept = (msg.selected || []).filter((i) => canKeepDie(msg.dice || [], i));
        const facesChanged = state._facesKey !== facesKey;
        state._facesKey = facesKey;
        if (facesChanged || !isPickingKeep(msg)) {
          state.selected = new Set(kept);
        }
      }
      state.game = msg;
      state.reconnecting = false;
      applyInvite(msg.code, msg.invite_path);
      rememberRoom(msg.code);
      renderGame();
      renderTimer();
      break;
    default:
      break;
  }
}

function leaveTable() {
  send({ type: "leave_game" });
  goHome("");
}

function goHome(message) {
  state.game = null;
  state.boundCode = null;
  state.selected = new Set();
  state.reconnecting = false;
  forgetRoom();
  history.replaceState(null, "", "/");
  const wrap = $("join-code-wrap");
  if (wrap) wrap.classList.add("hidden");
  const codeInput = $("join-code");
  if (codeInput) codeInput.value = "";
  showScreen("home");
  if (message) {
    showHomeError(message);
    toast(message);
  }
}

function showHomeError(text) {
  $("home-error").textContent = text || "";
}

function showScreen(id) {
  $("screen-home").classList.toggle("hidden", id !== "home");
  $("screen-game").classList.toggle("hidden", id !== "game");
}

function renderGame() {
  const g = state.game;
  if (!g) return;
  showScreen("game");
  applyInvite(g.code, g.invite_path);
  const showInvite = g.phase === "lobby";
  document.querySelectorAll(".invite-bar").forEach((el) => {
    el.classList.toggle("hidden", !showInvite);
  });
  $("screen-game")?.classList.toggle("in-play", !showInvite);
  $("status").textContent = g.message || "";
  renderTurnBanner(g);
  renderScoreboard(g);
  updateTurnScore(g);
  renderDice(g);
  renderActions(g);
  renderSettings(g);
}

function renderScoreboard(g) {
  const board = $("scoreboard");
  const actor = actingPlayer(g);
  const players = g.players || [];
  const seen = new Set();
  players.forEach((p, i) => {
    seen.add(p.id);
    let chip = board.querySelector(`[data-player-id="${p.id}"]`);
    if (!chip) {
      chip = document.createElement("div");
      chip.dataset.playerId = p.id;
      chip.innerHTML = `
        <span class="pinfo">
          <span class="pname"></span>
          <span class="pscore"></span>
        </span>
        <span class="badges"></span>
      `;
    }
    if (board.children[i] !== chip) {
      board.insertBefore(chip, board.children[i] || null);
    }
    const isActing = !!(actor && p.id === actor.id && !p.forfeited);
    chip.className = "player-chip";
    if (isActing) chip.classList.add("active");
    if (p.on_board && !p.forfeited) chip.classList.add("on-board");
    if (!p.on_board) chip.classList.add("off-board");
    if (p.forfeited) chip.classList.add("forfeited");
    const you = p.id === g.you_are ? " (you)" : "";
    const nameEl = chip.querySelector(".pname");
    const scoreEl = chip.querySelector(".pscore");
    const nameText = `${p.name}${you}`;
    if (nameEl.textContent !== nameText) nameEl.textContent = nameText;
    const scoreText = String(p.score);
    if (scoreEl.textContent !== scoreText) scoreEl.textContent = scoreText;
    const badges = [
      p.forfeited ? '<span class="badge forfeit">forfeited</span>' : "",
      p.connected ? "" : '<span class="badge">away</span>',
      g.winner_id === p.id ? '<span class="badge winner">winner</span>' : "",
    ].join("");
    const badgesEl = chip.querySelector(".badges");
    if (badgesEl.innerHTML !== badges) badgesEl.innerHTML = badges;
  });
  [...board.children].forEach((el) => {
    if (!seen.has(el.dataset.playerId)) el.remove();
  });
}

function scoreHeld(faces) {
  const counts = [0, 0, 0, 0, 0, 0, 0];
  for (const face of faces) {
    if (face >= 1 && face <= 6) counts[face] += 1;
  }
  for (let face = 1; face <= 6; face++) {
    if (counts[face] === 5) {
      return { points: face === 1 ? 2000 : 0, autoWin: face !== 1 };
    }
  }
  let points = 0;
  for (let face = 1; face <= 6; face++) {
    let c = counts[face];
    if (!c) continue;
    if (c >= 4) {
      points += face * 1000;
      c -= 4;
    } else if (c >= 3) {
      points += face === 1 ? 1000 : face * 100;
      c -= 3;
    }
    if (face === 1) points += c * 100;
    else if (face === 5) points += c * 50;
  }
  return { points, autoWin: false };
}

function heldFaces(g) {
  return [...state.selected].map((i) => g.dice[i]).filter((f) => f >= 1 && f <= 6);
}

function updateTurnScore(g) {
  const tp = $("turn-points");
  if (g.phase !== "playing" && g.phase !== "steal_window") {
    tp.classList.add("hidden");
    return;
  }
  tp.classList.remove("hidden");
  let shown = g.turn_points;
  if (g.awaiting_keep && !g.bust) {
    const held = scoreHeld(heldFaces(g));
    if (held.autoWin) {
      tp.innerHTML = `Turn: <strong>${g.turn_points}</strong> · five of a kind wins`;
      return;
    }
    shown += held.points;
  }
  tp.innerHTML = `Turn: <strong>${shown}</strong>`;
  if (g.pending_bank) {
    tp.innerHTML += ` · pending bank <strong>${g.pending_bank.points}</strong>`;
  }
}

function diceSelectable(g) {
  return (
    g.you_can_act &&
    g.phase === "playing" &&
    g.awaiting_keep &&
    !g.bust &&
    g.dice.length > 0
  );
}

function isPickingKeep(g) {
  return diceSelectable(g);
}

function selectedSet(g) {
  if (isPickingKeep(g)) return state.selected;
  return new Set((g.selected || []).filter((i) => canKeepDie(g.dice || [], i)));
}

function canKeepDie(dice, index) {
  const face = dice[index];
  if (face === 1 || face === 5) return true;
  if (face < 2 || face > 6) return false;
  return dice.filter((f) => f === face).length >= 3;
}

function syncDieAppearance(g) {
  const selectable = diceSelectable(g);
  const selected = selectedSet(g);
  const root = $("dice");
  [...root.children].forEach((btn, i) => {
    const keepable = canKeepDie(g.dice, i);
    btn.classList.toggle("selected", selected.has(i) && keepable);
    btn.classList.toggle("bust", !!g.bust);
    btn.classList.toggle("dead", !!g.bust || (!keepable && !selected.has(i)));
    btn.disabled = !selectable || (!keepable && !selected.has(i));
  });
}

function renderDice(g) {
  const root = $("dice");
  const facesKey = (g.dice || []).join(",");
  const facesChanged = state._renderedFaces !== facesKey;

  if (facesChanged) {
    const hadDice = state._renderedFaces != null && state._renderedFaces !== "";
    const land = hadDice || (state._renderedFaces != null && facesKey !== "");
    root.replaceChildren();
    g.dice.forEach((face, i) => {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "die" + (land && facesKey ? " land" : "");
      btn.dataset.face = String(face);
      btn.setAttribute("aria-label", `Die showing ${face}`);
      const faceEl = document.createElement("span");
      faceEl.className = "die-face";
      faceEl.appendChild(dieFaceSvg(face));
      btn.appendChild(faceEl);
      btn.addEventListener("click", () => {
        if (state.selected.has(i)) {
          state.selected.delete(i);
        } else {
          if (btn.disabled) return;
          if (!canKeepDie(g.dice, i)) return;
          state.selected.add(i);
        }
        syncDieAppearance(state.game);
        renderActions(state.game);
        updateTurnScore(state.game);
        send({
          type: "select",
          indices: [...state.selected].sort((a, b) => a - b),
        });
      });
      root.appendChild(btn);
    });
    state._renderedFaces = facesKey;
  }

  syncDieAppearance(g);
}

function renderActions(g) {
  const root = $("actions");
  const specs = actionSpecs(g);
  const existing = new Map([...root.children].map((el) => [el.dataset.key, el]));
  const used = new Set();
  specs.forEach((spec, i) => {
    used.add(spec.key);
    let el = existing.get(spec.key);
    if (!el) {
      el = spec.kind === "note" ? document.createElement("p") : document.createElement("button");
      el.dataset.key = spec.key;
      if (spec.kind === "button") {
        el.type = "button";
        el.addEventListener("click", onActionClick);
      }
    }
    if (spec.kind === "note") {
      if (el.textContent !== spec.text) el.textContent = spec.text;
    } else {
      el.dataset.type = spec.type;
      el.className = "btn " + spec.className;
      if (el.textContent !== spec.label) el.textContent = spec.label;
      el.disabled = spec.disabled;
    }
    if (root.children[i] !== el) {
      root.insertBefore(el, root.children[i] || null);
    }
  });
  for (const [key, el] of existing) {
    if (!used.has(key)) el.remove();
  }
}

function actionSpecs(g) {
  const specs = [];
  const btn = (key, label, type, opts = {}) => {
    specs.push({
      key,
      kind: "button",
      label,
      type,
      className: opts.className || "primary",
      disabled: !!opts.disabled,
    });
  };
  const note = (key, text) => {
    specs.push({ key, kind: "note", text });
  };

  if (g.phase === "lobby") {
    if (g.you_are === g.host_id) {
      btn("start", "Start game", "start_game", { disabled: g.players.length < 2 });
    } else {
      note("wait-host", "Waiting for host to start…");
    }
    return specs;
  }

  if (g.phase === "finished") {
    if (g.you_are === g.host_id) btn("rematch", "Rematch", "rematch");
    btn("leave", "Leave table", "leave_table", { className: "ghost" });
    return specs;
  }

  const me = g.players.find((p) => p.id === g.you_are);
  if (me?.forfeited) {
    note("forfeit", "You forfeited — watching.");
    btn("leave", "Leave table", "leave_table", { className: "ghost" });
    return specs;
  }

  if (g.phase === "steal_window") {
    if (g.steal_available) {
      btn("steal", "Steal!", "steal");
      btn("decline", "Let them keep it", "decline_steal", { className: "ghost" });
    } else {
      note("wait-steal", "Waiting on the next player…");
    }
  } else if (!g.you_can_act) {
    note("wait-turn", "Waiting for your turn…");
  } else if (g.awaiting_keep) {
    const held = scoreHeld(heldFaces(g));
    const canScore = held.points > 0 || held.autoWin;
    btn("roll", "Roll", "roll", { disabled: !canScore });
    btn("bank", "Bank", "bank", { className: "ghost", disabled: !canScore });
  } else {
    btn("roll", "Roll", "roll");
  }
  return specs;
}

function onActionClick(ev) {
  const type = ev.currentTarget.dataset.type;
  if (!type || ev.currentTarget.disabled) return;
  if (type === "leave_table") {
    leaveTable();
    return;
  }
  const indices = [...state.selected].sort((a, b) => a - b);
  if (type === "roll" || type === "bank") {
    send({ type, indices });
  } else if (type === "keep") {
    send({ type: "keep", indices });
  } else {
    send({ type });
  }
}

function renderSettings(g) {
  const menu = $("game-settings");
  const endBtn = $("settings-end-game");
  const forfeitBtn = $("settings-forfeit");
  if (!menu || !endBtn || !forfeitBtn) return;
  const me = g.players.find((p) => p.id === g.you_are);
  const inPlay = g.phase === "playing" || g.phase === "steal_window";
  const canEnd = inPlay && g.you_are === g.host_id;
  const canForfeit = inPlay && !me?.forfeited;
  endBtn.classList.toggle("hidden", !canEnd);
  forfeitBtn.classList.toggle("hidden", !canForfeit);
  menu.classList.toggle("hidden", !canEnd && !canForfeit);
  if (!canEnd && !canForfeit) menu.open = false;
}

function actingPlayer(g) {
  if (!g?.players?.length) return null;
  if (g.phase === "steal_window") {
    const cur = g.players.findIndex((p) => p.id === g.current_player_id);
    const start = cur >= 0 ? cur : 0;
    for (let step = 1; step <= g.players.length; step++) {
      const p = g.players[(start + step) % g.players.length];
      if (p && !p.forfeited) return p;
    }
  }
  if (g.phase === "playing") {
    return g.players.find((p) => p.id === g.current_player_id && !p.forfeited) || null;
  }
  return null;
}

function possessive(name) {
  const n = String(name || "Player");
  return n.endsWith("s") || n.endsWith("S") ? `${n}'` : `${n}'s`;
}

function renderTurnBanner(g) {
  const el = $("turn-banner");
  if (!el) return;
  const actor = actingPlayer(g);
  if (!actor || (g.phase !== "playing" && g.phase !== "steal_window")) {
    el.classList.add("hidden");
    return;
  }
  const yours = actor.id === g.you_are;
  el.classList.remove("hidden");
  el.classList.toggle("yours", yours);
  el.classList.toggle("theirs", !yours);
  if (g.phase === "steal_window") {
    el.textContent = yours ? "Your steal" : `${actor.name} may steal`;
  } else {
    el.textContent = yours ? "Your turn" : `${possessive(actor.name)} turn`;
  }
}

function renderTimer() {
  const el = $("turn-timer");
  if (!el) return;
  const g = state.game;
  if (!g || !g.action_deadline_ms || (g.phase !== "playing" && g.phase !== "steal_window")) {
    el.classList.add("hidden");
    return;
  }
  const secs = Math.max(0, Math.ceil((g.action_deadline_ms - Date.now()) / 1000));
  const m = Math.floor(secs / 60);
  const s = String(secs % 60).padStart(2, "0");
  const actor = actingPlayer(g);
  const yours = actor && actor.id === g.you_are;
  el.classList.remove("hidden");
  el.classList.toggle("urgent", secs <= 15);
  if (yours) {
    el.textContent = `You have ${m}:${s}`;
  } else if (actor) {
    el.textContent = `${actor.name} has ${m}:${s}`;
  } else {
    el.textContent = `${m}:${s}`;
  }
}

async function boot() {
  seatKey();
  $("player-name").value = state.playerName;

  const code = lastRoomCode();
  if (code) {
    $("join-code-wrap").classList.remove("hidden");
    $("join-code").value = code;
    applyInvite(code);
  }

  $("home-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    showHomeError("");
    const name = $("player-name").value.trim();
    if (!name) return;
    state.playerName = name;
    localStorage.setItem("bones-name", name);

    const intent = e.submitter?.value || "create";
    try {
      await connect();
      if (intent === "create") {
        send({ type: "leave_game" });
        state.game = null;
        state.boundCode = null;
        forgetRoom();
        send({ type: "create_game", name, seat_key: rotateSeatKey() });
        return;
      }
      let joinCode = ($("join-code").value || pathCode()).trim().toUpperCase();
      if (!joinCode) {
        $("join-code-wrap").classList.remove("hidden");
        $("join-code").focus();
        showHomeError("Enter a game code");
        return;
      }
      send({ type: "join_game", code: joinCode, name, seat_key: seatKey() });
      rememberRoom(joinCode);
      applyInvite(joinCode);
    } catch (err) {
      showHomeError(err.message || "Connection failed");
    }
  });

  $("leave-table")?.addEventListener("click", leaveTable);

  $("settings-end-game")?.addEventListener("click", () => {
    if (!window.confirm("End the game now? Highest score on the board wins.")) return;
    send({ type: "end_game" });
    const menu = $("game-settings");
    if (menu) menu.open = false;
  });
  $("settings-forfeit")?.addEventListener("click", () => {
    if (!window.confirm("Forfeit this game? You will be out for the rest of the match.")) return;
    send({ type: "forfeit" });
    const menu = $("game-settings");
    if (menu) menu.open = false;
  });

  $("copy-link").addEventListener("click", async () => {
    const code = state.game?.code || $("room-code").textContent;
    const url =
      $("invite-url").value ||
      inviteUrlFor(code);
    if (!url.includes("/g/")) {
      toast("No room code yet");
      return;
    }
    const ok = await copyText(url);
    if (ok) toast(`Copied ${url}`);
    else {
      $("invite-url").focus();
      $("invite-url").select();
      toast("Select and copy the invite link");
    }
  });

  setInterval(renderTimer, 250);

  const onForeground = () => {
    if (document.visibilityState === "visible") resumeIfNeeded();
  };
  document.addEventListener("visibilitychange", onForeground);
  window.addEventListener("pageshow", onForeground);
  window.addEventListener("online", onForeground);

  if (code && state.playerName) resumeIfNeeded();
}

boot();
