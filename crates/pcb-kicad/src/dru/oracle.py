"""Report whether KiCad's rule loader accepts each rule file.

Each file is followed by a rule that always fails in the loader's last pass, so
the loader stops with an error either way and never goes on to run DRC, which
standalone Python cannot do. The error names that rule only when everything
before it was accepted.

usage: oracle.py BOARD CASES.json ACCEPTED.json
"""

import json
import os
import sys
import tempfile

import pcbnew

SENTINEL = """
(rule "pcb-sentinel" (condition "ZZ.Type == 'x'") (constraint clearance (min 1mm)))
"""
PREFIX = "Init DRC engine: err <"

board_path, cases_path, accepted_path = sys.argv[1:]
rules_path = os.path.splitext(board_path)[0] + ".kicad_dru"
board = pcbnew.LoadBoard(board_path)
accepted = []
with tempfile.TemporaryDirectory() as tmp, open(cases_path, encoding="utf-8") as f:
    capture = os.path.join(tmp, "stderr")
    for rules in json.load(f):
        with open(rules_path, "w", encoding="utf-8", newline="") as out:
            out.write(rules + SENTINEL)
        saved = os.dup(2)
        fd = os.open(capture, os.O_WRONLY | os.O_CREAT | os.O_TRUNC)
        os.dup2(fd, 2)
        try:
            loaded = pcbnew.WriteDRCReport(
                board, os.path.join(tmp, "report"), pcbnew.EDA_UNITS_MM, False
            )
        finally:
            os.dup2(saved, 2)
            os.close(fd)
            os.close(saved)
        with open(capture, encoding="utf-8", errors="replace") as err:
            message = err.read().partition(PREFIX)[2]
        if loaded or not message:
            sys.exit(f"KiCad did not report a rule error for:\n{rules}")
        accepted.append("pcb-sentinel" in message)
with open(accepted_path, "w", encoding="utf-8") as f:
    json.dump(accepted, f)
