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

function wsUrl() {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  return `${proto}://${location.host}/ws`;
}

function connect() {
  return new Promise((resolve, reject) => {
    if (state.ws && state.ws.readyState === WebSocket.OPEN) {
      resolve(state.ws);
      return;
    }
    const ws = new WebSocket(wsUrl());
    state.ws = ws;
    ws.addEventListener("open", () => resolve(ws));
    ws.addEventListener("error", () => reject(new Error("Could not connect")));
    ws.addEventListener("message", (ev) => {
      try {
        onServer(JSON.parse(ev.data));
      } catch (e) {
        console.error(e);
      }
    });
    ws.addEventListener("close", () => {
      if (state.ws !== ws) return;
      if (state.game?.code) scheduleReconnect();
      else toast("Disconnected — refresh to rejoin");
    });
  });
}

function scheduleReconnect() {
  if (state.reconnecting) return;
  state.reconnecting = true;
  toast("Connection lost — reclaiming your seat…");
  const attempt = async (n) => {
    try {
      await connect();
      await new Promise((r) => setTimeout(r, 80));
      const code = state.game?.code || pathCode();
      const name = state.playerName || "Player";
      if (code) {
        send({ type: "join_game", code, name, seat_key: seatKey() });
      }
      state.reconnecting = false;
    } catch {
      setTimeout(() => attempt(n + 1), Math.min(1000 * 2 ** n, 15000));
    }
  };
  setTimeout(() => attempt(0), 400);
}

function send(msg) {
  if (state.ws?.readyState === WebSocket.OPEN) {
    state.ws.send(JSON.stringify(msg));
  }
}

function onServer(msg) {
  switch (msg.type) {
    case "welcome":
      state.playerId = msg.player_id;
      break;
    case "error":
      showHomeError(msg.message);
      toast(msg.message);
      break;
    case "game_created":
    case "joined":
      applyInvite(msg.code, msg.invite_path);
      localStorage.setItem("bones-room", msg.code);
      break;
    case "state":
      {
        const facesKey = (msg.dice || []).join(",");
        if (state._facesKey !== facesKey) {
          state.selected = new Set(msg.selected || []);
          state._facesKey = facesKey;
        }
      }
      state.game = msg;
      applyInvite(msg.code, msg.invite_path);
      localStorage.setItem("bones-room", msg.code);
      renderGame();
      renderTimer();
      break;
    default:
      break;
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
  $("status").textContent = g.message || "";

  const board = $("scoreboard");
  board.innerHTML = "";
  for (const p of g.players) {
    const chip = document.createElement("div");
    chip.className = "player-chip";
    if (p.id === g.current_player_id && !p.forfeited) chip.classList.add("active");
    if (!p.on_board) chip.classList.add("off-board");
    if (p.forfeited) chip.classList.add("forfeited");
    const you = p.id === g.you_are ? " (you)" : "";
    chip.innerHTML = `
      <span class="pname">${escapeHtml(p.name)}${you}</span>
      <span class="pscore">${p.score}</span>
      ${p.forfeited ? '<span class="badge forfeit">forfeited</span>' : ""}
      ${p.on_board || p.forfeited ? "" : '<span class="badge">off board</span>'}
      ${!p.connected ? '<span class="badge">away</span>' : ""}
      ${g.winner_id === p.id ? '<span class="badge">winner</span>' : ""}
    `;
    board.appendChild(chip);
  }

  updateTurnScore(g);
  renderDice(g);
  renderActions(g);
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

function syncDieAppearance(g) {
  const selectable = diceSelectable(g);
  const root = $("dice");
  [...root.children].forEach((btn, i) => {
    btn.classList.toggle("selected", state.selected.has(i));
    btn.classList.toggle("bust", !!g.bust);
    btn.disabled = !selectable;
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
        if (btn.disabled) return;
        if (state.selected.has(i)) state.selected.delete(i);
        else state.selected.add(i);
        syncDieAppearance(state.game);
        renderActions(state.game);
        updateTurnScore(state.game);
      });
      root.appendChild(btn);
    });
    state._renderedFaces = facesKey;
  }

  syncDieAppearance(g);
}

function renderActions(g) {
  const root = $("actions");
  root.innerHTML = "";

    const add = (label, type, opts = {}) => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "btn " + (opts.className || "primary");
    b.textContent = label;
    b.disabled = !!opts.disabled;
    b.addEventListener("click", () => {
      if (type === "end_game") {
        if (!window.confirm("End the game now? Highest score on the board wins.")) {
          return;
        }
        send({ type: "end_game" });
        return;
      }
      if (type === "forfeit") {
        if (!window.confirm("Forfeit this game? You will be out for the rest of the match.")) {
          return;
        }
        send({ type: "forfeit" });
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
    });
    root.appendChild(b);
  };

  if (g.phase === "lobby") {
    if (g.you_are === g.host_id) {
      add("Start game", "start_game", { disabled: g.players.length < 2 });
    } else {
      const wait = document.createElement("p");
      wait.textContent = "Waiting for host to start…";
      root.appendChild(wait);
    }
    return;
  }

  if (g.phase === "finished") {
    if (g.you_are === g.host_id) add("Rematch", "rematch");
    return;
  }

  const me = g.players.find((p) => p.id === g.you_are);
  if (me?.forfeited) {
    const wait = document.createElement("p");
    wait.textContent = "You forfeited — watching.";
    root.appendChild(wait);
    return;
  }

  if (g.phase === "steal_window") {
    if (g.steal_available) {
      add("Steal!", "steal");
      add("Let them keep it", "decline_steal", { className: "ghost" });
    } else {
      const wait = document.createElement("p");
      wait.textContent = "Waiting on the next player…";
      root.appendChild(wait);
    }
  } else if (!g.you_can_act) {
    const wait = document.createElement("p");
    wait.textContent = "Waiting for your turn…";
    root.appendChild(wait);
  } else if (g.awaiting_keep) {
    const held = scoreHeld(heldFaces(g));
    const canScore = held.points > 0 || held.autoWin;
    add("Roll", "roll", { disabled: !canScore });
    add("Bank", "bank", { className: "ghost", disabled: !canScore });
  } else {
    add("Roll", "roll");
  }

  if (g.you_are === g.host_id) {
    add("End game", "end_game", { className: "danger" });
  }
  add("Forfeit", "forfeit", { className: "ghost" });
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
  el.classList.remove("hidden");
  el.classList.toggle("urgent", secs <= 15);
  el.textContent = g.you_can_act ? `Play within ${m}:${s}` : `Turn timer ${m}:${s}`;
}

function escapeHtml(s) {
  return String(s)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

async function boot() {
  seatKey();
  $("player-name").value = state.playerName;

  const code = pathCode();
  if (code) {
    $("join-code-wrap").classList.remove("hidden");
    $("join-code").value = code;
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
      if (!state.ws || state.ws.readyState !== WebSocket.OPEN) {
        await connect();
        await new Promise((r) => setTimeout(r, 80));
      }
      if (intent === "create") {
        send({ type: "create_game", name, seat_key: seatKey() });
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
    } catch (err) {
      showHomeError(err.message || "Connection failed");
    }
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

  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible" && state.game?.code) {
      if (!state.ws || state.ws.readyState !== WebSocket.OPEN) {
        scheduleReconnect();
      }
    }
  });

  if (code && state.playerName) {
    try {
      await connect();
      await new Promise((r) => setTimeout(r, 80));
      send({
        type: "join_game",
        code,
        name: state.playerName,
        seat_key: seatKey(),
      });
    } catch (err) {
      showHomeError(err.message || "Connection failed");
    }
  }
}

boot();
