#!/usr/bin/env python3
"""Compare `pcbc step export` with `kicad-cli pcb export step` over a corpus.

    scripts/corpus.py <corpus root or boards.txt> <results dir> [--runs 3]

Every `.kicad_pcb` under the root with an `Edge.Cuts` item is exported by
both tools with default options; pcbc is timed best of `--runs`, kicad-cli
once. The OCCT oracle compares each pair. Results accumulate in
`results.jsonl` so an interrupted run resumes, and a summary is printed.
Needs `uv` for the oracle and kicad-cli on the default macOS path or
`--kicad-cli`.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import pathlib
import statistics
import subprocess
import time

HERE = pathlib.Path(__file__).resolve().parent
SKIP = {".pcb", "vendor", "node_modules", ".history", "build"}

ap = argparse.ArgumentParser(
    description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
)
ap.add_argument("corpus", type=pathlib.Path)
ap.add_argument("results", type=pathlib.Path)
ap.add_argument(
    "--pcbc", default=str(HERE.parent.parent.parent / "target/release/pcbc")
)
ap.add_argument(
    "--kicad-cli", default="/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"
)
ap.add_argument("--runs", type=int, default=3)
args = ap.parse_args()
out = args.results / "out"
out.mkdir(parents=True, exist_ok=True)
log = args.results / "results.jsonl"


def boards():
    if args.corpus.is_file():
        return [l.strip() for l in args.corpus.read_text().splitlines() if l.strip()]
    found = []
    for root, dirs, files in os.walk(args.corpus):
        dirs[:] = [d for d in dirs if d not in SKIP and not d.startswith("_restore")]
        for f in files:
            p = pathlib.Path(root) / f
            if f.endswith(".kicad_pcb") and '(layer "Edge.Cuts")' in p.read_text(
                errors="replace"
            ):
                found.append(str(p))
    return sorted(found)


def timed(cmd, timeout):
    t = time.perf_counter()
    try:
        p = subprocess.run(
            cmd, capture_output=True, text=True, timeout=timeout, check=False
        )
        code = p.returncode
    except subprocess.TimeoutExpired:
        code = 124
    return time.perf_counter() - t, code


def oracle(ours, ref):
    p = subprocess.run(
        [
            "uv",
            "run",
            "--with",
            "cadquery",
            str(HERE / "oracle.py"),
            str(ours),
            str(ref),
        ],
        capture_output=True,
        text=True,
        timeout=3600,
        check=False,
    )
    return p.returncode == 0, [
        l for l in p.stdout.splitlines() if l.startswith("MISMATCH")
    ][:8]


done = (
    {json.loads(l)["board"] for l in log.read_text().splitlines()}
    if log.exists()
    else set()
)
todo = [b for b in boards() if b not in done]
print(f"{len(todo)} boards to run, {len(done)} done", flush=True)
for i, board in enumerate(todo):
    name = board.strip("/").replace("/", "__")
    ours, ref = out / f"{name}.pcbc.step", out / f"{name}.cli.step"
    runs, code = [], 0
    for _ in range(args.runs):
        wall, code = timed([args.pcbc, "step", "export", board, "-o", str(ours)], 600)
        runs.append(wall)
        if code != 0:
            break
    cli, cli_code = timed(
        [args.kicad_cli, "pcb", "export", "step", "--force", "-o", str(ref), board],
        1800,
    )
    rec = {
        "board": board,
        "pcb_bytes": os.path.getsize(board),
        "pcbc_s": min(runs),
        "pcbc_code": code,
        "cli_s": cli,
        "cli_code": cli_code,
        "pcbc_bytes": ours.stat().st_size if ours.exists() else 0,
        "cli_bytes": ref.stat().st_size if ref.exists() else 0,
    }
    if ours.exists() and ref.exists():
        rec["match"], rec["mismatches"] = oracle(ours, ref)
    else:
        rec["match"], rec["mismatches"] = False, ["missing output"]
    with log.open("a") as f:
        f.write(json.dumps(rec) + "\n")
    print(
        f"{i + 1}/{len(todo)} {name[-60:]} pcbc={rec['pcbc_s']:.3f}s cli={cli:.2f}s "
        f"{'match' if rec['match'] else 'DIFF ' + '; '.join(rec['mismatches'])[:120]}",
        flush=True,
    )

recs = [json.loads(l) for l in log.read_text().splitlines()]
paired = [
    r for r in recs if r["pcbc_code"] == 0 and r["cli_code"] == 0 and r["pcbc_s"] > 0
]
ratios = [r["cli_s"] / r["pcbc_s"] for r in paired]
sizes = [r["pcbc_bytes"] / r["cli_bytes"] for r in paired if r["cli_bytes"]]
gm = lambda xs: math.exp(statistics.fmean(map(math.log, xs))) if xs else float("nan")
print(f"\n{len(recs)} boards, {len(paired)} exported by both")
print(
    f"speedup: geometric mean {gm(ratios):.0f}x, median {statistics.median(ratios):.0f}x, "
    f"range {min(ratios):.0f}x to {max(ratios):.0f}x"
)
print(
    f"total: pcbc {sum(r['pcbc_s'] for r in paired):.1f} s, kicad-cli {sum(r['cli_s'] for r in paired):.0f} s"
)
print(f"output size ratio: geometric mean {gm(sizes):.3f}")
print(f"oracle: {sum(r['match'] for r in recs)} of {len(recs)} match")
for r in recs:
    if not r["match"]:
        print(f"  {r['board']}: {'; '.join(r['mismatches'])[:160]}")
