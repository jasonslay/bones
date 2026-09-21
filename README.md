# Bones

A multiplayer web dice table for **Bones** (5 dice) and **Farkle** (6 dice). Create a room, pick a game and board threshold, copy the invite link (`/g/ABC12`), and roll.

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

- Host picks **Bones** or **Farkle** in the lobby, the score needed to get on the board (defaults: 1,000 / 500), and can set or disable idle forfeit (off by default).
- 1s = 100, 5s = 50; 3 of a kind = face × 100; 4 of a kind = face × 200; three 1s = 1000; five 1s = 2000.
- Bones: five of a kind (faces 2–6) wins instantly.
- Farkle extras: three pairs = 1500, straight 1–6 = 1500, two triplets = 2500; 5 / 6 of a kind = face × 300 / 400 (1s: 3000 / 4000).
- Once on the board, steal leftover dice from the player before you.
- First to **exactly 10,000** wins.
