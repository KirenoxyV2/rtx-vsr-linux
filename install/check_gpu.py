#!/usr/bin/env python3
"""
check_gpu.py — hardware gate for rtx-vsr-linux.

Answers the question "can THIS machine run VSR comfortably?" using only
nvidia-smi (no Python GPU deps required), so it can run before the venv is
even built. The proxy / installer calls this and gates on the verdict.

Verdicts:
  supported     — any RTX card: Turing (20xx, sm_75) or newer. The bundled
                  model ships sm_75/80/86/89 kernels; every mode was measured
                  on an RTX 2060 SUPER (spike/results_turing.txt).
  unsupported   — no NVIDIA GPU, pre-Turing, or driver too old.

Exit codes: 0 = supported, 20 = unsupported.
Prints a human-readable report to stdout and a machine-readable JSON line
to stderr (so callers can parse it without scraping the prose).
"""
import json
import re
import subprocess
import sys

# Minimum driver branches that ship the VSR-capable VFX runtime on Linux.
MIN_DRIVER_BRANCHES = {570: 190, 580: 82, 590: 44}


def run_nvidia_smi(*args):
    try:
        out = subprocess.run(
            ["nvidia-smi", *args],
            capture_output=True, text=True, timeout=15,
        )
    except FileNotFoundError:
        return None
    if out.returncode != 0:
        return None
    return out.stdout


def parse_driver_version(smi_out):
    m = re.search(r"Driver Version:\s*([\d.]+)", smi_out)
    if not m:
        return (0, 0)
    parts = m.group(1).split(".")
    major = int(parts[0]) if len(parts) > 0 else 0
    minor = int(parts[1]) if len(parts) > 1 else 0
    return (major, minor)


def driver_ok(ver):
    major, minor = ver
    if major in MIN_DRIVER_BRANCHES:
        return minor >= MIN_DRIVER_BRANCHES[major]
    # A newer branch than any we know about is fine.
    return major > max(MIN_DRIVER_BRANCHES)


def parse_compute_cap(cc_str):
    """'8.9' -> 89 (sm_89). Returns None on parse failure."""
    try:
        a, b = cc_str.split(".")
        return int(a) * 10 + int(b)
    except Exception:
        return None


def main():
    smi_out = run_nvidia_smi()
    if smi_out is None:
        report("unsupported", "nvidia-smi not found — no NVIDIA driver present")
        return

    # Query per-GPU name + compute capability.
    q = run_nvidia_smi(
        "--query-gpu=index,name,compute_cap,memory.total,driver_version",
        "--format=csv,noheader,nounits",
    )
    gpus = []
    if q:
        for line in q.strip().splitlines():
            idx, name, cc, mem, _drv = [c.strip() for c in line.split(",")]
            gpus.append({
                "index": int(idx),
                "name": name,
                "compute_cap": cc,
                "sm": parse_compute_cap(cc),
                "vram_mb": int(mem) if mem.isdigit() else None,
            })

    drv_ver = parse_driver_version(smi_out)
    drv_ok = driver_ok(drv_ver)

    # Classify each GPU.
    for g in gpus:
        sm = g["sm"]
        if sm is None:
            g["class"] = "unknown"
        elif sm >= 75:          # Turing (20xx), Ampere (30xx), Ada (40xx), ...
            g["class"] = "supported"
        else:
            g["class"] = "unsupported"

    if not gpus:
        report("unsupported", "nvidia-smi present but no GPU enumerated")
        return

    # Overall verdict: supported if ANY gpu is supported.
    classes = {g["class"] for g in gpus}
    verdict = "supported" if "supported" in classes else "unsupported"

    # Build the human report.
    lines = []
    lines.append("rtx-vsr-linux hardware check")
    lines.append("=" * 40)
    lines.append(f"Driver version : {drv_ver[0]}.{drv_ver[1]}"
                 f"   (need >= one of {MIN_DRIVER_BRANCHES})   "
                 f"[{'OK' if drv_ok else 'TOO OLD'}]")
    for g in gpus:
        tag = {"supported": "SUPPORTED",
               "unsupported": "UNSUPPORTED",
               "unknown": "UNKNOWN"}[g["class"]]
        vram = f"{g['vram_mb']} MB" if g.get("vram_mb") else "?"
        lines.append(f"  GPU{g['index']}: {g['name']}  sm_{g['sm']}  "
                     f"{vram} VRAM  -> {tag}")
    lines.append("-" * 40)
    if verdict == "supported":
        lines.append("VERDICT: supported — RTX GPU detected.")
        if all(g["sm"] < 80 for g in gpus if g["class"] == "supported"):
            lines.append("NOTE: Turing (RTX 20xx) runs every mode, at roughly a "
                         "tenth of a 4090's speed: prefer LOW/MEDIUM for 60 fps "
                         "or 4K output (HIGH/ULTRA hold ~30 fps at 1080p input).")
    else:
        lines.append("VERDICT: unsupported — no RTX (Turing or newer) GPU found.")
    if not drv_ok:
        lines.append("NOTE: driver is below the minimum VFX branch; update it "
                     "(570.190 / 580.82 / 590.44+) before installing.")

    print("\n".join(lines))
    # Machine-readable line on stderr for callers.
    print(json.dumps({
        "verdict": verdict,
        "driver": f"{drv_ver[0]}.{drv_ver[1]}",
        "driver_ok": drv_ok,
        "gpus": gpus,
    }), file=sys.stderr)

    return {"supported": 0, "unsupported": 20}[verdict]


def report(verdict, reason):
    print(f"rtx-vsr-linux hardware check\n{'='*40}\n{reason}")
    print("-" * 40)
    print(f"VERDICT: {verdict}")
    print(json.dumps({"verdict": verdict, "reason": reason}), file=sys.stderr)
    sys.exit({"supported": 0, "unsupported": 20}[verdict])


if __name__ == "__main__":
    sys.exit(main())
