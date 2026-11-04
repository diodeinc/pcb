"""Check the benchmark report's public CLI, including noise and incomplete comparisons."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

REPORT = Path(__file__).resolve().parents[1] / "pcb-build-perf-report"


@pytest.mark.parametrize(
    "head_median,base_stddev,head_stddev,change",
    [
        (1.25, 0.01, 0.02, "**+25.0%**"),
        (0.75, 0.01, 0.02, "**-25.0%**"),
        (1.04, 0, 0, "+4.0%"),
        (1.05, 0, 0, "**+5.0%**"),
        (1.18, 0.06, 0.08, "+18.0%"),
        (1.22, 0.06, 0.08, "**+22.0%**"),
        # A delta exactly twice the combined noise is not significant.
        (1.25, 0.125, 0, "+25.0%"),
    ],
)
def test_report_significance(tmp_path, head_median, base_stddev, head_stddev, change):
    for revision, median, stddev in [
        ("base", 1, base_stddev),
        ("head", head_median, head_stddev),
    ]:
        (tmp_path / f"Workspace-{revision}.json").write_text(
            json.dumps(
                {
                    "boards": [
                        {
                            "zen_path": "board/board.zen",
                            "name": "Controller",
                            "median_seconds": median,
                            "stddev_seconds": stddev,
                        }
                    ]
                }
            )
        )
        # A later unchanged workspace must neither hide an earlier change nor be omitted.
        (tmp_path / f"ZUnchanged-{revision}.json").write_text(
            json.dumps(
                {
                    "boards": [
                        {
                            "zen_path": "board.zen",
                            "name": "ZUnchanged",
                            "median_seconds": 2,
                            "stddev_seconds": 0,
                        }
                    ]
                }
            )
        )
    result = subprocess.run(
        [sys.executable, str(REPORT), str(tmp_path)],
        check=True,
        capture_output=True,
        text=True,
    )
    report = result.stdout
    assert f"| Workspace/Controller | 1000ms ±{base_stddev * 1000:.1f}" in report
    assert (
        f" | {head_median * 1000:.0f}ms ±{head_stddev * 1000:.1f} | {change} |"
        in report
    )
    assert "| ZUnchanged | 2000ms ±0.0 | 2000ms ±0.0 | +0.0% |" in report


@pytest.mark.parametrize("missing", ["results", "revision", "board"])
def test_report_rejects_incomplete_comparison(tmp_path, missing):
    if missing != "results":
        (tmp_path / "Workspace-base.json").write_text(
            json.dumps({"boards": [{"zen_path": "board.zen"}]})
        )
    if missing == "board":
        (tmp_path / "Workspace-head.json").write_text(json.dumps({"boards": []}))
    result = subprocess.run(
        [sys.executable, str(REPORT), str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode != 0
    assert "Expected matching base/head" in result.stderr
