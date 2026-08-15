const DIE_FACES = ["", "1", "2", "3", "4", "5", "6"];

const state = {
  ws: null,
  playerId: null,
  playerName: localStorage.getItem("bones-name") || "",
  seatKey: null,
  game: null,
  selected: new Set(),
  joinMode: false,
  reconnecting: false,
};

const $ = (id) => document.getElementById(id);

function seatKey() {
  if (state.seatKey) return state.seatKey;
  let key = localStorage.getItem("bones-seat");
  if (!key) {
    key = crypto.randomUUID();
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
  toast._t = setTimeout(() => el.classList.add("hidden"), 2800);
}

/** Works on plain HTTP (clipboard API needs a secure context). */
async function copyText(text) {
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // fall through — common on http://LAN IPs
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
      if (state.game?.code) {
        scheduleReconnect();
      } else {
        toast("Disconnected — refresh to rejoin");
      }
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
      await new Promise((r) => setTimeout(r, 100));
      const code = state.game?.code || pathCode();
      const name = state.playerName || "Player";
      if (code) {
        send({
          type: "join_game",
          code,
          name,
          seat_key: seatKey(),
        });
      }
      state.reconnecting = false;
    } catch {
      const delay = Math.min(1000 * 2 ** n, 15000);
      setTimeout(() => attempt(n + 1), delay);
    }
  };
  setTimeout(() => attempt(0), 500);
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
      history.replaceState(null, "", `/g/${msg.code}`);
      localStorage.setItem("bones-room", msg.code);
      break;
    case "state":
      state.game = msg;
      localStorage.setItem("bones-room", msg.code);
      if (msg.phase !== undefined) {
        const key = `${msg.dice.join(",")}|${msg.selected.join(",")}|${msg.turn_points}|${msg.phase}`;
        if (state._diceKey !== key) {
          state.selected = new Set(msg.selected || []);
          state._diceKey = key;
        }
      }
      renderGame();
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
  $("room-code").textContent = g.code;
  const invite = `${location.origin}/g/${g.code}`;
  const inviteInput = $("invite-url");
  if (inviteInput && inviteInput.value !== invite) {
    inviteInput.value = invite;
  }
  $("status").textContent = g.message || "";

  const board = $("scoreboard");
  board.innerHTML = "";
  for (const p of g.players) {
    const chip = document.createElement("div");
    chip.className = "player-chip";
    if (p.id === g.current_player_id) chip.classList.add("active");
    if (!p.on_board) chip.classList.add("off-board");
    const you = p.id === g.you_are ? " (you)" : "";
    chip.innerHTML = `
      <span class="pname">${escapeHtml(p.name)}${you}</span>
      <span class="pscore">${p.score}</span>
      ${p.on_board ? "" : '<span class="badge">off board</span>'}
      ${!p.connected ? '<span class="badge">away</span>' : ""}
      ${g.winner_id === p.id ? '<span class="badge">winner</span>' : ""}
    `;
    board.appendChild(chip);
  }

  const tp = $("turn-points");
  if (g.phase === "playing" || g.phase === "steal_window") {
    tp.classList.remove("hidden");
    tp.innerHTML = `Turn: <strong>${g.turn_points}</strong>`;
    if (g.pending_bank) {
      tp.innerHTML += ` · pending bank <strong>${g.pending_bank.points}</strong>`;
    }
  } else {
    tp.classList.add("hidden");
  }

  renderDice(g);
  renderActions(g);
}

function renderDice(g) {
  const root = $("dice");
  root.innerHTML = "";
  const selectable =
    g.you_can_act && g.phase === "playing" && g.awaiting_keep && g.dice.length > 0;

  g.dice.forEach((face, i) => {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "die" + (state.selected.has(i) ? " selected" : "");
    btn.textContent = DIE_FACES[face] || String(face);
    btn.disabled = !selectable;
    btn.addEventListener("click", () => {
      if (!selectable) return;
      if (state.selected.has(i)) state.selected.delete(i);
      else state.selected.add(i);
      renderDice(g);
      renderActions(g);
    });
    root.appendChild(btn);
  });
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
      if (type === "keep") {
        send({ type: "keep", indices: [...state.selected].sort((a, b) => a - b) });
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
    if (g.you_are === g.host_id) {
      add("Rematch", "rematch");
    }
    return;
  }

  if (g.phase === "steal_window") {
    if (g.steal_available) {
      add("Steal!", "steal", { className: "primary" });
      add("Let them keep it", "decline_steal", { className: "ghost" });
    } else {
      const wait = document.createElement("p");
      wait.textContent = "Waiting on the next player…";
      root.appendChild(wait);
    }
    return;
  }

  // playing
  if (!g.you_can_act) {
    const wait = document.createElement("p");
    wait.textContent = "Waiting for your turn…";
    root.appendChild(wait);
    return;
  }

  if (g.awaiting_keep) {
    add("Keep selected", "keep", { disabled: state.selected.size === 0 });
  } else {
    add("Roll", "roll");
    if (g.turn_points > 0) {
      add("Bank", "bank", { className: "ghost" });
    }
  }
}

function escapeHtml(s) {
  return String(s)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function pathCode() {
  const m = location.pathname.match(/^\/g\/([A-Za-z0-9]+)/i);
  return m ? m[1].toUpperCase() : "";
}

async function boot() {
  seatKey();
  $("player-name").value = state.playerName;

  const code = pathCode();
  if (code) {
    state.joinMode = true;
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
        state.joinMode = false;
        send({ type: "create_game", name, seat_key: seatKey() });
        return;
      }

      let joinCode = ($("join-code").value || pathCode()).trim().toUpperCase();
      if (!joinCode) {
        state.joinMode = true;
        $("join-code-wrap").classList.remove("hidden");
        $("join-code").focus();
        showHomeError("Enter a game code");
        return;
      }
      send({
        type: "join_game",
        code: joinCode,
        name,
        seat_key: seatKey(),
      });
    } catch (err) {
      showHomeError(err.message || "Connection failed");
    }
  });

  $("copy-link").addEventListener("click", async () => {
    const g = state.game;
    if (!g) return;
    const url = `${location.origin}/g/${g.code}`;
    const ok = await copyText(url);
    if (ok) {
      toast("Invite link copied");
      return;
    }
    const input = $("invite-url");
    if (input) {
      input.focus();
      input.select();
    }
    toast("Select and copy the invite link");
  });

  // Returning to an invite URL (or after sleep/refresh): reclaim seat automatically
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
