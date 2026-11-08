# Evasion research notes (2025–2026)

Deep-dive findings that shaped this repo. Each section ends with the concrete
decision we made in code. Treat this as the honest map: what works, what is
already detected, and what is a dead end.

---

## 0. TL;DR

- **Reflectively loading the Rust/tokio implant is unsound.** Rust uses static
  TLS callbacks to bootstrap its runtime; a manually mapped image has no TLS
  index/thread slots, so the OS loader cannot initialize it — and tokio's worker
  threads make it worse. Every serious Rust PE loader hits this.
- **Default strategy is now temp-exec** (OS loader runs the decoded payload from
  a delete-pending `%TEMP%` file → correct TLS/SEH). Reflective stays available
  (`--reflective`) for TLS-free payloads.
- **Entrypoint AMSI patching is a detection**, not a bypass, since Defender
  ~1.419.240.0. Do not write `C3`/`mov eax; ret` into `AmsiScanBuffer`. Our
  implant doesn't host script engines, so it doesn't need AMSI at all.
- **Sleep obfuscation (Ekko/Foliage/Cronos) is also detected** by memory scanners
  (APC/timer frames, `CONTEXT` scans, `RSP`/TEB checks, ETW-TI fluctuation).
- **Direct/indirect syscalls** defeat userland hooks but not kernel callbacks,
  ETW-TI, or call-stack validation. They raise cost, not immunity.
- **No crypter guarantees FUD.** Screen-read + input-injection is itself a strong
  RAT behavior signal. Plan for a signed binary / mature C2 for real work.

---

## 1. Reflective PE loading and Rust's TLS (critical)

Manual mapping must do everything the Windows loader does: headers, sections,
DIR64 relocations, imports, **delayed imports**, **exception tables (.pdata)**,
**TLS**, and optional **CFG** registration.

The TLS part is what breaks Rust:

- Rust's runtime registers **TLS callbacks** and uses static TLS (`.tls`). The
  Windows loader assigns a TLS **slot index** at load time into
  `IMAGE_TLS_DIRECTORY.AddressOfIndex`, allocates a per-thread TLS block, copies
  the module's default TLS data, and stores the block in
  `TEB->ThreadLocalStoragePointer[index]`.
- A manual mapper that only calls the callbacks with `DLL_PROCESS_ATTACH` (what
  this repo originally did) does **not** allocate the index or per-thread data.
  Multi-threaded Rust binaries then crash or misbehave when a thread touches a
  `thread_local!`. Reference: `Fatmike-GH/PELoader` explicitly documents that its
  earlier loader "was unable to load both single-threaded and multi-threaded Rust
  executables. This limitation is due to Rust's use of Thread Local Storage (TLS)
  callbacks to initialize its runtime environment," and describes the full
  fix (index allocation + per-thread copy + thread create/detach handling via a
  TLS callback proxy).
- tokio adds worker threads (`DLL_THREAD_ATTACH` semantics) that a one-shot
  mapper never sees.

**Decision:** loader supports two strategies chosen at pack time:
- `temp-exec` (default): decrypt → `%TEMP%\<rand>.exe` → `CreateProcess`
  (`DETACHED_PROCESS`) → unlink immediately (delete-pending). The OS loader does
  TLS/SEH/CFG correctly because the payload is a real image. Persistence is
  installed for the **loader**, not the plaintext payload.
- `reflective` (`--reflective`): map in memory. Add `RtlAddFunctionTable` for
  `.pdata` so SEH works. Still TLS-limited; only for TLS-free/native payloads or
  a future loader with full TLS-slot emulation + thread tracking.

Other loader gaps we closed/have on the map:
- ✅ x64 exception table registration (`RtlAddFunctionTable`).
- ⬜ delayed imports (DLLs almost never rely on this in our payloads).
- ⬜ security cookie (`__security_cookie`) init for `/GS` payloads.
- ⬜ CFG: `SetProcessValidCallTargets` (`CFG_CALL_TARGET_INFO`) if the payload is
  `/guard:cf`. Private `VirtualAlloc` pages are allowed by default non-strict
  CFG, so this is usually unnecessary for mapped private memory.
  Reference: `Thoxy67/rspe`, `neohook` cfg module, `mmap_inject`.
- ⬜ static TLS slot + per-thread TLS + thread create/detach tracking.

---

## 2. AMSI / ETW patching is now a detection

- Since ~2024 (Defender engine ~1.419.240.0) Microsoft added behavioral detection
  for the classic `AmsiScanBuffer` entrypoint patch. Writing a `ret` (`C3`) or
  `mov eax, 0x80070057; ret` into the function triggers a memory scan/alert.
  Sources: Practical Security Analytics, GlobeTech ("The RETurn of AMSI"), r-tec.
- The write is visible via **ETW-TI** (kernel-sourced), which userland patching
  cannot hide. CISA countermeasure CM0110 explicitly monitors `AmsiScanBuffer`
  integrity.
- More OpSec-safe alternatives if AMSI must be bypassed: **hardware breakpoints**
  + VEH, **page-guard** VEH (`PAGE_EXECUTE_READ | PAGE_GUARD`), or **VEH²**
  (CrowdStrike 2025). These still have ETW-TI `SetThreadContext` tells.
- ETW userland patch (`EtwEventWrite` → `ret`) silences *userland* providers but
  **not** ETW-TI or kernel callbacks.

**Decision:** our implant is a native process, not a script host, so AMSI is
irrelevant. `evasion.rs::patch_amsi` stays as reference code but is **not wired**
and must not be used as a "bypass". The ETW userland patch remains optional and
is documented as partial (does not stop ETW-TI).

---

## 3. Direct / indirect syscalls (Hell's Gate → Halo's → Tartarus)

- The Gate family resolves SSNs (`mov eax, imm`) from `ntdll` at runtime:
  - **Hell's Gate**: reads the stub directly — breaks when `ntdll` is hooked.
  - **Halo's Gate**: if hooked (`E9`), walks neighbors (±32 bytes) and derives
    the SSN by index distance.
  - **Tartarus' Gate**: also handles prologue-split hooks / hooked 4th byte.
  - **FreshyCalls/SysWhispers**: sorted export counting.
- **Direct syscall** (`syscall` in your own `.text`) is flagged by call-stack
  analysis: the syscall return address is in your module, not `ntdll`.
- **Indirect syscall**: jump to a `syscall; ret` gadget inside `ntdll` so the
  stack looks legitimate. Still detectable: spoofing mismatches (a different
  `Nt*` frame on the stack), `RSP` outside TEB stack bounds, non-module-backed
  frames.
- Kernel callbacks (`PsSetCreateProcessNotifyRoutineEx`, `ObRegisterCallbacks`,
  `CmRegisterCallbackEx`) and **ETW-TI** fire regardless of how the syscall
  entered. VBS/HVCI/CET raise the cost.

**Decision:** not implemented in this repo. It is a large, arch-specific,
untestable-here change and does not fix kernel-side telemetry. Documented as the
next hardening step; a mature C2 (Havoc/Sliver) already ships it.

---

## 4. Sleep obfuscation (Ekko / Foliage / Cronos) and its tells

- **Ekko**: timer-queue callbacks + `NtContinue` ROP → RC4 encrypt `.text` (via
  `SystemFunction032`), flip to `RW`, sleep, decrypt, flip back to `RX`.
- **Foliage**: APC chain to a helper thread, fake `CONTEXT` with `Rip` in `ntdll`
  and a synthetic stack in `kernel32`, so the sleeping thread's stack looks
  legitimate.
- **Cronos**: waitable timers + `NtContinue`, RC4 via advapi.

Scanners detect these by:
- `KiUserApcDispatcher` / `RtlpTpTimerCallback` frames during sleep.
- Pre-built `CONTEXT` structs in memory with `Rip` → `NtProtectVirtualMemory`.
- `RSP` outside the TEB stack bounds; cloned thread `CONTEXT` (same TIB).
- `NtProtectVirtualMemory(RW) → sleep → (RX)` toggle on private memory (ETW-TI).
- Memory-scan fluctuation: executable private memory present one scan, gone the
  next.
Sources: RedCell (2025), Binary Defense, maldev.lol (2026), sillywa.re,
Moneta/Patriot tooling.

**Decision:** not implemented. The current implant beacons at a low, tunable FPS
and doesn't hide its image. If a sleep mask is added, prefer a design that also
avoids the known tells (no pre-built CONTEXTs, no APC/timer frames) — which is
research-grade, not a weekend patch.

---

## 5. Behavioral detection of this specific RAT

Screen capture + synthetic input is a textbook remote-access pattern:

- Repeated GDI `BitBlt` / `GetDIBits` of the desktop DC, or DXGI duplication.
- `SendInput` / `mouse_event` / `SetCursorPos` injecting input while no human is
  at the console.
- Persistent HKCU Run + hidden EXE in `%APPDATA%\Microsoft\...`.
- Outbound WebSocket to a fixed path with periodic JPEG-sized bursts.

Mitigations that matter more than byte-level obfuscation:
- Use a **signed** binary (valid cert in scope) and a mature C2's malleable
  profile; avoid a fixed `ws` path and fixed beacon cadence.
- Prefer **DXGI Desktop Duplication** over GDI (less classic-capture signature,
  better perf, multi-monitor/idle handling).
- Gate capture on real input/idle; cap FPS; avoid touching
  `%APPDATA%\Microsoft` (heavily monitored) — use a less obvious path.
- Sleep mask + syscalls only help if implemented well (see §3–§4); half-measures
  add signatures.

---

## 6. C2 transport

- Domain fronting is largely dead on major CDNs; prefer **redirectors**,
  **malleable profiles**, and a domain you control with TLS (`wss://`).
- Terminate TLS at your edge (nginx/Cloudflare Tunnel) and keep the backend on
  localhost. Use a real cert; never ship a self-signed cert that mismatches.
- Add jittered beaconing, per-session tokens, and rotate endpoints via DNS.

---

## 7. What changed in this repo as a result

- `crypter/loader`: **two strategies**; temp-exec default (reliable for
  Rust/tokio), reflective opt-in.
- `crypter/loader`: reflective path now registers **x64 exception tables**
  (`RtlAddFunctionTable`) so SEH/panics can unwind.
- `crypter/loader`: temp-exec installs **loader persistence** and launches the
  payload with `--no-install`, so the plaintext payload is never copied to
  `%APPDATA%`.
- `implant`: honors `--no-install`.
- `crypter/packer`: `--reflective` flag; emits `REFLECTIVE` into `gen.rs`;
  self-verifies the encrypted blob.
- Documented AMSI/ETW/syscall/sleep-obf reality instead of pretending.

---

## 8. Deliberately not done (and why)

| Technique | Why not (here) |
|---|---|
| Full TLS-slot emulation for reflective Rust | Needs per-thread TLS + thread create/detach tracking; untestable here; temp-exec is correct today. |
| Direct/indirect syscalls | Large arch-specific code; doesn't stop kernel callbacks/ETW-TI; mature C2s have it. |
| Sleep obfuscation (Ekko/Foliage) | Detected by modern scanners; half-implementations add signatures. |
| AMSI entrypoint patch | Now a detection; our payload doesn't need AMSI. |
| CFG registration | Non-strict CFG allows private executable pages; add only if payload is `/guard:cf`. |
| Signed binary / cert | Out of scope for a lab repo; required for real engagements. |

---

## References

- Fatmike-GH/PELoader — Rust TLS in manual mapping: https://github.com/Fatmike-GH/PELoader
- Thoxy67/rspe — PE loader pipeline (delayed imports, RtlAddFunctionTable, CFG): https://github.com/Thoxy67/rspe
- mmap_inject — manual mapping with TLS callbacks/SEH/header wiping: https://crates.io/crates/mmap_inject
- neohook CFG module — `SetProcessValidCallTargets` details: https://docs.rs/neohook
- Hell's/Halo's/Tartarus comparison: https://tyrusrc.github.io/learnfromscratch/learn/topics/red-team/hells-halos-tartarus-gates-comparison/
- Detecting direct/indirect syscalls (Oblivion, 2025): https://oblivion-malware.xyz/posts/detecting-syscalls/
- Alt syscalls for Windows 11 (fluxsec): https://fluxsec.red/alt-syscalls-for-windows-11
- Sleep obfuscation overview (RedCell, 2025): https://redcelllearning.com/blog/sleep-obfuscation-implant-memory
- Sleep obfuscation retrospective (maldev.lol, 2026): https://maldev.lol/posts/sleepy-beacons-retrospective/
- Ekko/Foliage/Cronos analysis (Binary Defense): https://binarydefense.com/resources/blog/understanding-sleep-obfuscation
- Defender AMSI patch detection (Practical Security Analytics): https://practicalsecurityanalytics.com/obfuscating-api-patches-to-bypass-new-windows-defender-behavior-signatures/
- "The RETurn of AMSI" (GlobeTech, 2025): https://globetech.biz/blog/the-return-of-amsi-easy-dll-patching-without-c3/
- AMSI bypass in 2025 (r-tec): https://www.r-tec.net/r-tec-blog-bypass-amsi-in-2025.html
- Patchless AMSI / VEH² (CrowdStrike, 2025): https://www.crowdstrike.com/en-us/blog/crowdstrike-investigates-threat-of-patchless-amsi-bypass-attacks/
- CISA CM0110 (AMSI bypass countermeasures): https://www.cisa.gov/eviction-strategies-tool/info-countermeasures/CM0110
