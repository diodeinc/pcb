"""Run with uv run --locked pytest bin/tests."""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import kicad_ipc_gerber_svg_diff as diff


def drill(metadata, body):
    return f"M48\n{metadata}\nMETRIC\nT1C0.6\n%\nT1\n{body}\nM30\n"


def header(function):
    # Native KiCad X2-compatible Excellon comment, also used by XNC.
    return f"; #@! TF.FileFunction,{function}"


@pytest.fixture
def compare(tmp_path, monkeypatch):
    def run(reference, candidate):
        def export(_command):
            for name, text in reference.items():
                (tmp_path / "kicad-drills" / name).write_text(text)

        monkeypatch.setattr(diff, "run", export)
        ipc = tmp_path / "ipc"
        ipc.mkdir()
        for name, text in candidate.items():
            (ipc / name).write_text(text)
        return diff.compare_drills("kicad-cli", Path("board.kicad_pcb"), tmp_path, ipc)

    return run


@pytest.mark.parametrize(
    "first,second",
    [
        ("Plated,1,4,PTH", "NonPlated,1,4,NPTH"),
        ("Plated,1,2,Blind", "Plated,1,4,Blind"),
        ("Plated,2,5,Buried", "Plated,3,5,Buried"),
    ],
)
@pytest.mark.parametrize(
    "body", ["X10.0Y20.0", "G00X10.0Y20.0\nM15\nG01X12.0Y23.0\nM16"]
)
def test_swapped_groups_cannot_cancel(compare, first, second, body):
    # Both exports have identical geometry, group sets and per-group counts.
    # Only the association of geometry to plating/span differs.
    other = body.replace("10.0", "30.0").replace("12.0", "32.0")
    result = compare(
        {"a.drl": drill(header(first), body), "b.drl": drill(header(second), other)},
        {"a.drl": drill(header(second), body), "b.drl": drill(header(first), other)},
    )
    assert result.failed()
    assert len(result.missing) == len(result.extra) == 2
    plating, start, end, _ = first.split(",")
    assert any(f"{plating} layers {start}-{end}:" in s for s in result.missing)


def test_same_group_matches_across_files_and_reversed_slot_endpoints(compare):
    metadata = header("Plated,1,4,PTH")
    result = compare(
        {
            "native.drl": drill(
                metadata,
                "X10.0Y20.0\nX30.0Y40.0\nG00X11.0Y22.0\nM15\nG01X14.0Y27.0\nM16",
            )
        },
        {
            # Filenames deliberately give the wrong plating and no span.
            "NPTH.drl": drill(metadata, "X30.005Y40.0\nX14.0Y27.0G85X11.0Y22.0"),
            "unrelated.drl": drill(metadata, "X10.0Y20.0"),
        },
    )
    assert result == diff.DrillResult(2, 2, 1, 1, [], [])


def test_missing_group_reports_geometry(compare):
    result = compare({"a.drl": drill(header("NonPlated,1,4,NPTH"), "X10.0Y20.0")}, {})
    assert result.missing == ["NonPlated layers 1-4: hole d=0.600 at (10.000, 20.000)"]
    assert not result.extra


@pytest.mark.parametrize(
    "metadata",
    [
        "",
        header("MixedPlating,1,4"),
        header("Plated,0,4,PTH"),
        header("Plated,4,1,PTH"),
        header("Plated,1,1,PTH"),
        header("Plated,1,4,PTH") + "\n" + header("NonPlated,1,4,NPTH"),
    ],
)
def test_unknown_metadata_fails_even_when_both_files_agree(compare, metadata, capsys):
    text = drill(metadata, "X10.0Y20.0")
    with pytest.raises(SystemExit) as error:
        compare({"PTH.drl": text}, {"PTH.drl": text})
    assert error.value.code == 2
    assert "PTH.drl" in capsys.readouterr().err
