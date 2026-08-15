# Bones

A multiplayer web version of **Bones** — a high-risk dice game. Create a room, share the link, and roll.

## Run

```bash
cargo run --release
```

Open [http://localhost:8080](http://localhost:8080). Bind address defaults to `0.0.0.0:8080` (override with `BONES_ADDR`).

## Stack

- **Bevy** (headless) — room entities and turn resolution
- **Axum** — HTTP + WebSocket, static web UI
- Shareable room links: `/g/ABC12`

## Rules (summary)

- Five dice. Score on 1s (100) and 5s (50), three/four of a kind, three/five 1s, etc.
- Five of a kind (faces 2–6) wins instantly.
- Need **1,000 in one turn** to get on the board.
- Once on the board, you may **steal** leftover dice from the player before you.
- First to **exactly 10,000** wins.
