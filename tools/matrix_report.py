#!/usr/bin/env python3
"""Aggregate vm_run_variant.ps1 JSON reports into a markdown matrix.

Usage: python3 matrix_report.py <results_dir> [out.md]
"""
import glob
import json
import sys
from pathlib import Path


def load(path):
    try:
        with open(path, encoding="utf-8-sig") as f:
            return json.load(f)
    except Exception as exc:
        return {"variant": Path(path).name, "error": str(exc)}


def short(sha):
    return (sha or "-")[:12]


def det_names(dets):
    if not dets:
        return "-"
    names = []
    for d in dets:
        if isinstance(d, dict):
            names.append(str(d.get("Threat") or d.get("threat") or d.get("ThreatID") or "?"))
        else:
            names.append(str(d))
    return ", ".join(names)


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    src = Path(sys.argv[1])
    out = Path(sys.argv[2]) if len(sys.argv) > 2 else None
    files = sorted(glob.glob(str(src / "result-*.json")))
    rows = []
    detected = alive = 0
    for f in files:
        r = load(f)
        defender = r.get("defender") or {}
        dets = r.get("detections") or []
        if dets:
            detected += 1
        if r.get("process_alive"):
            alive += 1
        rows.append(
            (
                r.get("variant", Path(f).stem),
                short(r.get("sha256")),
                r.get("arguments", ""),
                "yes" if r.get("process_alive") else "no",
                defender.get("RealTimeProtectionEnabled", "?"),
                defender.get("IsTamperProtected", "?"),
                det_names(dets),
            )
        )
    lines = [
        "| Variant | SHA256 | Args | Alive | RTP | Tamper | Detections |",
        "|---|---|---|---|---|---|---|",
    ]
    for row in rows:
        lines.append("| " + " | ".join(str(c) for c in row) + " |")
    if not rows:
        lines.append("| (no result-*.json found) | - | - | - | - | - | - |")
    md = "\n".join(lines) + "\n"
    print(f"variants: {len(rows)}  detected: {detected}  alive: {alive}")
    print(md)
    if out:
        out.write_text(md, encoding="utf-8")
        print(f"[+] written: {out}")


if __name__ == "__main__":
    main()
