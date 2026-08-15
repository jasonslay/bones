# Bones

A multiplayer web version of **Bones** — a 5-dice high-risk game (not 6-dice Farkle). Create a room, copy the invite link (`/g/ABC12`), and roll.

## Run

```bash
cargo run --release
```

Binds to `0.0.0.0:8080` by default (`BONES_ADDR` to override). Open that host from other devices on the network.

Invite links always include the room code, e.g. `http://10.1.20.28:8080/g/ABC12`.

Production: [https://bones.jtslay.com](https://bones.jtslay.com) (`ghcr.io/jasonslay/bones`).

## Stack

- Rust 2024, **Bevy 0.19.1** (headless rooms/turns)
- Axum HTTP + WebSocket
- Static UI in `web/`

## Rules (summary)

- Five dice. 1s = 100, 5s = 50; three/four of a kind; three 1s = 1000; five 1s = 2000.
- Five of a kind (faces 2–6) wins instantly.
- Need **1,000 in one turn** to get on the board.
- Once on the board, steal leftover dice from the player before you.
- First to **exactly 10,000** wins.
