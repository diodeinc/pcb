"""Exercise the release installer with local artifacts and no network access."""

from __future__ import annotations

import hashlib
import platform
import shutil
import subprocess
import sys
from pathlib import Path

import pytest


@pytest.mark.skipif(sys.platform != "linux", reason="Linux desktop integration")
@pytest.mark.parametrize(
    "xdg_mode,registration_status", [(None, 0), (0o644, 0), (0o755, 0), (0o755, 1)]
)
def test_installer_skips_unusable_desktop_tools(
    tmp_path, xdg_mode, registration_status
):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    for tool in [
        "uname",
        "sed",
        "mktemp",
        "rm",
        "sha256sum",
        "mkdir",
        "chmod",
        "mv",
        "cp",
        "cat",
    ]:
        executable = shutil.which(tool)
        assert executable is not None
        (bin_dir / tool).symlink_to(executable)

    if xdg_mode is not None:
        xdg_mime = bin_dir / "xdg-mime"
        xdg_mime.write_text(
            '#!/bin/sh\nprintf registered > "$HOME/url-handler"\n'
            f"exit {registration_status}\n"
        )
        xdg_mime.chmod(xdg_mode)

    artifacts = tmp_path / "artifacts"
    artifacts.mkdir()
    target = f"{platform.machine()}-unknown-linux-musl"
    for binary, content in [
        ("pcb", b"#!/bin/sh\nexit 0\n"),
        (
            "pcb-launcher",
            b"#!/bin/sh\nxdg-mime default diode-pcb-launcher.desktop x-scheme-handler/diode\n",
        ),
    ]:
        name = f"{binary}-{target}"
        (artifacts / name).write_bytes(content)
        (artifacts / f"{name}.sha256").write_text(hashlib.sha256(content).hexdigest())

    # Only replace the download transport; run the entire installer unchanged.
    script = """
curl() {
  if [[ "$2" == */pcb-latest.json ]]; then
    printf '{"tag":"pcb/v0.2.8"}'
  else
    cp "$ARTIFACTS/${2##*/}" "$4"
  fi
}
export -f curl
exec "$BASH" "$1"
"""
    install_dir = tmp_path / "install"
    result = subprocess.run(
        [
            "/bin/bash",
            "-c",
            script,
            "installer-test",
            str(Path(__file__).resolve().parents[2] / "install.sh"),
        ],
        env={
            "HOME": str(tmp_path),
            "PATH": f"{bin_dir}:{install_dir}",
            "PCB_INSTALL_DIR": str(install_dir),
            "ARTIFACTS": str(artifacts),
        },
        capture_output=True,
        text=True,
        check=True,
    )

    assert (install_dir / "pcb").is_file()
    assert (tmp_path / "url-handler").exists() == (xdg_mode == 0o755)
    if xdg_mode == 0o755 and registration_status != 0:
        assert "could not register the Diode URL launcher" in result.stderr
    else:
        assert result.stderr == ""
