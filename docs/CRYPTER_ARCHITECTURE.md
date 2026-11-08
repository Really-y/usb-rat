# Crypter & RAT Research Architecture

Version: 2026-11 · Status: engineering documentation for an isolated lab study
Companion documents: `docs/TESTING.md` (methodology + measured results), `docs/EVASION.md` (2026 detection landscape), `README.md` (build/run), `PROTOCOL.md` (wire format).

---

## 1. Scope and lab policy

This document describes the architecture of a research crypter/implant stack
built to study executable protection, controlled credential-access behavior,
and antivirus telemetry under laboratory conditions. All experimentation is
assumed to occur inside a snapshotted, non-domain VM with no production data.
The stack is dual-use: every module (packer, loader, stagers, harness) is also
a legitimate software-protection component; the "harvest" and C2 modules exist
to exercise the exact behavior antivirus engines monitor.

---

## 2. System overview

```
                     BUILD TIME                                RUNTIME (Windows)
 ┌──────────────────────────────────────────┐   ┌──────────────────────────────────────┐
 │ .env (token, chat) ──► implant/build.rs  │   │  delivery                              │
 │   ──► cargo:rustc-env ──► option_env!()  │   │   USB: Sənədlər.exe (folder icon)      │
 │                                          │   │   stager.exe / wmpsvc.dll / wmpdui.dll │
 │ implant.exe (TG-only agent)              │   │                                        │
 │   │ crypter-packer                       │   │  loader (temp-exec, default no-persist)│
 │   ▼                                      │   │   1. ETW+AMSI patch                    │
 │ payload.enc  (XChaCha20-Poly1305)        │───┼─► 2. anti-analysis + delay             │
 │ gen.rs       (key, nonce, strategy)      │   │   3. decrypt (AEAD, tamper-checked)    │
 │   ▼ cargo build                          │   │   4. temp-exec → implant --no-install  │
 │ loader.exe  (temp-exec)                  │   │      or reflective map (in-process)    │
 │ wmpdui.dll  (reflective, DllMain+Run)    │   │                                        │
 │                                          │   │  implant (Telegram-only)               │
 │ donut: implant.exe ──► shellcode ──►     │   │   1. getUpdates long-poll (chat lock)  │
 │   XOR-encrypt ──► C stager               │   │   2. raw harvest: cmd.exe copies        │
 │   stager.exe / wmpsvc.dll (Run)          │   │      browser DBs; DPAPI unwraps key     │
 │   + ETW/AMSI patch                       │   │   3. tar.exe zips; curl.exe uploads     │
 │                                          │   │   4. keylog / screen / shell via chat   │
 └──────────────────────────────────────────┘   └──────────────────────────────────────┘
```

Two control planes exist: Telegram-only (primary, no WS C2) and the WebSocket
C2 server (fallback when `TG_TOKEN` is absent).

---

## 3. Module descriptions

| Module | Path | Role |
|---|---|---|
| Agent | `implant/` | Rust x64 agent. DPI-aware screen capture, remote input, shell, harvest, keylog, Telegram C2, persistence (opt-in) |
| Harvest | `implant/src/harvest.rs` | Raw browser artifact copy via `cmd.exe`, DPAPI key unwrap, wallets/keys/docs/wifi/clipboard/env collectors |
| Keylog | `implant/src/keylog.rs` | `GetAsyncKeyState` polling + `ToUnicodeEx`; 3 s flush to Telegram or C2 |
| Telegram | `implant/src/telegram.rs` | `curl.exe`-based Bot API client (obfuscated endpoints), ZIP exfil, chat lock |
| TG C2 | `implant/src/tg_c2.rs` | Long-poll command loop: screen/stream/shell/harvest/grab/keylog/clip/input |
| WS C2 | `implant/src/c2.rs`, `server/` | WebSocket relay + operator UI + loot store (non-Telegram fallback) |
| Packer | `crypter/packer/` | Encrypts a PE with a fresh key/nonce, writes `payload.enc` + `gen.rs`, self-verifies |
| Crypto core | `crypter/common/` | XChaCha20-Poly1305 AEAD (`encrypt`/`decrypt`), unit-tested |
| Loader (EXE) | `crypter/loader/` | temp-exec or reflective; ETW/AMSI patch; persistence opt-in (`--persist`) |
| Loader (DLL) | `crypter/dll-loader/` | `wmpdui.dll`: `DllMain` (2 s delayed thread) + `Run` export; single-run guard |
| Donut stager | `/tmp/opencode` build assets → `dist/stager.exe`, `dist/wmpsvc.dll` | C stub: XOR-decrypt Donut shellcode → RWX → `CreateThread`; ETW/AMSI patch; no temp file |
| Stager | `stager/build_stager.ps1` | USB `.lnk` lure + hidden `_sys` drop |
| Tools | `tools/` | `sideload.ps1` (scheduled-task persistence), `decrypt_chromium.py` (offline credential decryption) |

---

## 4. Cryptographic design

### 4.1 AEAD scheme

`crypter/common` implements **XChaCha20-Poly1305** (`chacha20poly1305` crate):

- 256-bit key, 192-bit nonce, 128-bit Poly1305 tag.
- Nonce-misuse resistance is not assumed: a **fresh random key+nonce per build**
  makes reuse negligible (both drawn from the OS RNG via `getrandom`).
- AEAD provides confidentiality **and** integrity: any modification of the
  ciphertext (or wrong key) fails authentication. This is the tamper-detection
  primitive for the packed payload.

Why not AES-GCM: XChaCha's extended nonce removes nonce-management risk and it
is constant-time in pure Rust without AES-NI dependency. The browser-credential
path uses AES-256-GCM only because Chrome's `v10` format demands it.

### 4.2 Key and nonce generation

```
getrandom(32 B key, 24 B nonce)  ──►  encrypt(payload)  ──►  payload.enc
                                        gen.rs: KEY/NONCE/REFLECTIVE/BLOB
```

- Packer **self-verifies**: decrypts the fresh blob and asserts equality with
  the plaintext before writing.
- `cargo build` embeds `gen.rs` (`include_bytes!`) into loader/DLL/stager.
- Per-build keying means every build has a different hash and no key material
  is shared across engagements.

### 4.3 Key management lifecycle

| Phase | Mechanism |
|---|---|
| Build secrets (Telegram) | `.env` (gitignored, mode 600) → `implant/build.rs` → `cargo:rustc-env` → `option_env!`; process env overrides file |
| Payload key/nonce | generated per pack; embedded next to the blob; regenerated per build |
| Runtime secret material | AES key for browser DBs derived at runtime via DPAPI (never stored by us); Telegram token lives in-process only |
| Rotation | re-pack + re-build; `.env` token rotation via BotFather |
| Key escrow | none — loss of `gen.rs`+`payload.enc` is intentional (no offline decryption of a delivered artifact) |
| Machine binding | `--bind <MachineGuid>`: key = HKDF-SHA256(secret, MachineGuid); only `SECRET` is embedded, the derived key is never stored; a different machine fails closed |

Machine binding (2026-11): `crypter-packer --bind <guid>` derives the payload
key from a random build secret and the target's
`HKLM\SOFTWARE\Microsoft\Cryptography\MachineGuid` via HKDF-SHA256. The loader
reads the GUID at runtime and derives the same key; a different machine cannot
decrypt. Verified: bound decrypt == implant PASS, wrong-GUID rejection PASS,
stored `KEY` is all zeros.

### 4.4 Browser credential cryptography (analysis)

- Chromium `Local State` → `os_crypt.encrypted_key` = base64("DPAPI"+blob).
- Agent unwraps the 32-byte AES key with `CryptUnprotectData` (current user).
- `v10`/`v11` values: `nonce[12] ‖ ciphertext ‖ tag[16]` under AES-256-GCM —
  decrypted **offline** by `tools/decrypt_chromium.py`.
- `v20` (App-Bound Encryption, Chrome 127+): cannot be unwrapped from a user
  process; rows are skipped, not corrupted.
- Firefox: raw profile shipped (`logins.json`, `key4.db`); NSS key derivation
  out of scope.

### 4.5 Integrity verification and tamper detection

| Layer | Mechanism | Effect on tamper |
|---|---|---|
| Packed payload | Poly1305 tag | wrong key/corruption → decrypt error → silent exit |
| Packer | encrypt→decrypt self-assert | key/nonce mismatches are impossible to ship |
| Loader | silent return on decrypt failure | no crash signature, no partial execution |
| Shellcode blob | XOR layer (integrity-checked at test time only) | obfuscation, not authentication (documented gap) |
| String layer | XOR-decoded endpoint strings | removes static signatures, not tamper-proof |

Shellcode layer (closed 2026-11): the Donut shellcode is encrypted with
XChaCha20-Poly1305 (32-byte key, 24-byte nonce; tag stored with the blob) and
decrypted inside the C stager by the embedded Monocypher implementation.
Authentication failure aborts silently. Cross-implementation verified on Linux:
`monocypher decrypt == shellcode` PASS, tamper rejected PASS.

### 4.6 Anti-modification of protected components

- PE headers overwritten by Donut; reflective mapping keeps the plaintext off
  disk; temp-exec uses a delete-pending `%TEMP%` name.
- Anti-debug (`IsDebuggerPresent`, `CheckRemoteDebuggerPresent`) + 8 s sandbox
  delay in the loader.
- ETW (`EtwEventWrite` → `ret`) and AMSI (`AmsiScanBuffer` → `E_INVALIDARG`)
  patches applied in the loader, reflective DLL, and Donut stager (strings
  XOR-obfuscated in the C stager).
- Runtime string obfuscation: Telegram endpoints and method names never appear
  in any artifact's plaintext.

---

## 5. Execution strategies

| Strategy | Disk | Process | Notes |
|---|---|---|---|
| temp-exec (`loader.exe`, default) | delete-pending `%TEMP%` exe | own (unsigned) | reliable TLS/SEH; plaintext briefly on disk |
| reflective (`wmpdui.dll`) | none | signed host (`rundll32`) | TLS-unsound for Rust/tokio — test per target |
| Donut shellcode (`wmpsvc.dll`, `stager.exe`) | none | signed host (`rundll32`) or stager process | mature PE loader; verified decrypt + export |
| indirect syscall + callback (`wmpsvc2.dll`, `stager2.exe`, experimental) | none | signed host (`rundll32`) | fresh-ntdll SSN/`syscall` resolution, targeted stub restore, `EnumWindows` callback (no `CreateThread`) |

Persistence: none by default (`--persist` opt-in). Operator installs a logon
Scheduled Task via `tools/sideload.ps1` (quieter than `HKCU\Run` + self-copy).

---

## 6. Stealth / telemetry layer (summary)

String hygiene (measured, see TESTING.md): Telegram endpoints/methods and
browser/persist literals (`Login Data`, `Cookies`, `WindowsRuntimeHost`, …)
are **0 across all artifacts** after the 2026-11 obfuscation pass; the only
residual `Google` occurrences are TLS root-store names (benign). Raw browser
exfil avoids in-process SQLite/AES behavior. `curl.exe`/`tar.exe`/`cmd.exe` are
signed carriers for network/ZIP. Browser copies are **direct `CopyFileExW`**
(Rust `fs::copy`): a normal harvest spawns **no `cmd.exe`** at all. The only
child processes are `tar.exe`/`curl.exe` (ZIP/upload) and
`netsh`/`powershell` (env collector); `/shell` uses `cmd.exe` only on demand. All child processes (`whoami`, `ipconfig`, `net`,
`tasklist`, `powershell`, `netsh`, `tar.exe`, `curl.exe`) are spawned with
`CREATE_NO_WINDOW`, so the GUI-subsystem agent never flashes a console window.
The loader no longer carries persistence code (no self-copy, no `HKCU\Run`);
persistence is installed externally by `tools/sideload.ps1` as a scheduled task.

---

## 7. Windows compatibility matrix

| Component | Win10 21H2+ | Win11 | Constraints |
|---|---|---|---|
| implant | expected | expected | GDI capture; `windows` 0.58 bindings; DPAPI current-user |
| loader / DLL | expected | expected | mingw/x64; ETW/AMSI patch paths guarded (no-op if absent) |
| Donut stager | expected | expected | RWX alloc subject to system DEP/CFG policy |
| curl/tar dependency | built-in 1803+ | built-in | PowerShell `Compress-Archive` fallback |
| Schtasks persistence | yes | yes | user-level task, no admin |
| BYOVD (optional) | admin | admin | HVCI and driver blocklist govern success |

Untested on this host: everything Windows-side is compiled and statically
verified only (see TESTING.md limitations).

---

## 8. Performance benchmarks (measured 2026-11, this host)

| Metric | Value |
|---|---|
| XChaCha20-Poly1305 decrypt | **~1800 MB/s** (500 × 1.34 MB in 0.35 s) |
| implant size (plaintext) | 1,341,440 B (entropy 6.52) |
| payload.enc | 1,341,456 B (entropy 8.00) |
| loader.exe | 1,712,640 B (entropy 7.87) |
| wmpdui.dll (reflective) | 1,625,088 B (entropy 7.90) |
| wmpsvc.dll (Donut stager, AEAD) | 1,424,384 B (entropy 7.99) |
| stager.exe (Donut stager, AEAD) | 1,428,480 B (entropy 7.99) |
| Build times (x86_64-pc-windows-gnu, release) | implant ~10–12 s, loader ~2 s, DLL ~2 s, C stager <2 s |

Decryption cost in the loader is negligible (<1 ms for the 1.3 MB payload);
startup is dominated by the 8 s sandbox delay.

---

## 9. Security assessment & limitations

1. **No guaranteed AV bypass.** Defender's cloud/behavior/ML and Tamper
   Protection defeated the first in-process-decrypting build
   (`PWS:MSIL/Polazert.GA!MTB`). The current raw-exfil + obfuscated-string
   build reduces triggers but is unverified end-to-end on Windows.
2. **Reflective Rust/tokio TLS risk.** `pe::map_into_self` and Donut both
   execute a Rust runtime in an unmapped-PE context; thread-local handling is
   the likely crash source. Fallbacks exist (temp-exec).
3. **Kernel telemetry remains.** ETW/AMSI patches are user-mode; kernel
   callbacks and ETW-TI still observe execution. Full blinding requires BYOVD.
4. **XOR shellcode layer** is obfuscation, not authentication (gap, §4.5).
5. **Static implant strings** (`Login Data`, persistence names) survive in the
   implant image; hidden inside packed blobs but visible after temp-exec.
6. **Key-at-rest in the artifact** — anyone with the loader can extract the
   payload (standard crypter property; no binding to the target machine).
7. **DPAPI/`v20` limits** — Chrome App-Bound rows are not recoverable here.
8. **Legal/authorization** — lab-only; no production use.

---

## 10. Implementation roadmap

1. **[DONE]** AEAD-encrypt the Donut shellcode blob (Monocypher; tamper-checked).
2. **[DONE]** Obfuscate implant browser/persist strings (0 residuals measured).
3. **[DONE, experimental]** Indirect syscalls + targeted ntdll stub restore in
   the C stager (`wmpsvc2.dll`): SSN/`syscall` resolution from the fresh on-disk
   ntdll, targeted 32-byte stub restore, `EnumWindows` callback execution, no
   `CreateThread`. Runtime status: needs a Windows VM run; fallback is
   `wmpsvc.dll`.
4. **[DONE]** Optional admin path: BYOVD kill/callback removal (snapshot VM only) — `tools/byovd_lab.ps1` (operator-supplied tool/driver).
5. **[DONE]** Machine binding: `--bind <MachineGuid>` derives the payload key
   with HKDF-SHA256; only the secret is embedded, the key is never stored.
6. **[DONE]** Windows-side automated re-test harness: `tools/vm_run_variant.ps1`
   (per-variant JSON) + `tools/matrix_report.py` (JSON → markdown matrix).

---

## 11. References

- `docs/EVASION.md` — 2026 detection landscape (BYOVD, sideloading, AMSI/ETW).
- `RESEARCH.md` (repo) — reflective TLS analysis.
- `PROTOCOL.md` — wire protocol.
- `chacha20poly1305` crate docs; RFC 8439; Chrome os_crypt format references.
