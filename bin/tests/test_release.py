"""Release contracts against a local Git remote and simulated AWS/GitHub services."""

from __future__ import annotations

import base64
import fnmatch
import hashlib
import importlib.util
import json
import shutil
import subprocess
import sys
from contextlib import nullcontext
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "pcb-release.py"
REAL_RUN = subprocess.run


def git(*args):
    return REAL_RUN(
        ["git", *args], check=True, text=True, capture_output=True
    ).stdout.strip()


@pytest.fixture
def release(tmp_path, monkeypatch):
    spec = importlib.util.spec_from_file_location("pcb_release", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    monkeypatch.chdir(tmp_path)
    monkeypatch.delenv("CARGO_TARGET_DIR", raising=False)
    monkeypatch.setenv("CARGO_INCREMENTAL", "0")
    monkeypatch.setenv("RUSTFLAGS", "")
    git("init", "-b", "main", ".")
    git("config", "user.name", "Release Test")
    git("config", "user.email", "release@example.invalid")
    Path(".gitignore").write_text("/target\n")
    Path("source").write_text("released source\n")
    git("add", ".")
    git("commit", "-m", "Source to release")
    remote = tmp_path.parent / (tmp_path.name + "-remote.git")
    git("init", "--bare", str(remote))
    git("remote", "add", "origin", str(remote))
    git("push", "origin", "main")
    module.ASSETS.mkdir(parents=True)
    return module


class Cloud:
    def __init__(self):
        self.objects = {}
        self.github = None
        self.fail_key = None
        self.fail_tag = False
        self.fail_asset = False
        self.denied = False
        self.writes = 0
        self.metadata = {}
        self.fail_artifact = None
        self.uploaded_artifacts = {}
        self.artifact_sources = None

    def run(self, args, **kwargs):
        def result(value=None, error=None):
            if error and kwargs.get("check"):
                raise subprocess.CalledProcessError(1, args, stderr=error)
            return subprocess.CompletedProcess(
                args,
                int(error is not None),
                json.dumps(value) if value is not None else "",
                error or "",
            )

        def flag(name):
            return args[args.index(name) + 1]

        if args[0] == "aws":
            if self.denied:
                return result(error="S3 (403) Forbidden")
            if args[2] == "list-objects-v2":
                keys = sorted(
                    key for key in self.objects if key.startswith(flag("--prefix"))
                )[:1]
                return result({"Contents": [{"Key": key} for key in keys]})
            key = flag("--key")
            previous = self.objects.get(key)
            if args[2] == "head-object":
                if previous is None:
                    return result(error="HeadObject (404) Not Found")
                return result(
                    {
                        "ContentLength": len(previous),
                        "ChecksumSHA256": base64.b64encode(
                            hashlib.sha256(previous).digest()
                        ).decode(),
                        "ETag": hashlib.md5(previous).hexdigest(),
                    }
                )
            if args[2] == "get-object":
                assert previous is not None
                assert flag("--if-match") == hashlib.md5(previous).hexdigest()
                Path(args[-1]).write_bytes(previous)
                return result({})
            assert args[2] == "put-object"
            if key == self.fail_key:
                return result(error="Simulated S3 outage")
            if "--if-none-match" in args and previous is not None:
                return result(error="PreconditionFailed")
            if "--if-match" in args and (
                previous is None
                or flag("--if-match") != hashlib.md5(previous).hexdigest()
            ):
                return result(error="PreconditionFailed")
            value = Path(flag("--body")).read_bytes()
            if "--checksum-sha256" in args:
                assert (
                    flag("--checksum-sha256")
                    == base64.b64encode(hashlib.sha256(value).digest()).decode()
                )
            self.objects[key] = value
            self.writes += 1
            return result({})
        if args[0] == "gh":
            if "--slurp" in args:
                return result([[self.github] if self.github else []])
            if args[1:3] == ("release", "upload"):
                path = Path(args[4])
                if self.fail_asset:
                    self.github["assets"].append(
                        {
                            "name": path.name,
                            "id": 456,
                            "state": "starter",
                            "size": 0,
                            "digest": None,
                        }
                    )
                    return result(error="Simulated GitHub upload outage")
                self.github["assets"].append(
                    {
                        "name": path.name,
                        "state": "uploaded",
                        "size": path.stat().st_size,
                        "digest": "sha256:"
                        + hashlib.sha256(path.read_bytes()).hexdigest(),
                    }
                )
            elif "DELETE" in args:
                assert args[2].endswith("/releases/assets/456")
                self.github["assets"] = [
                    asset for asset in self.github["assets"] if asset.get("id") != 456
                ]
            elif "POST" in args:
                self.github = {**json.loads(kwargs["input"]), "assets": [], "id": 123}
            elif "PATCH" in args:
                self.github.update(json.loads(kwargs["input"]))
            elif self.github is None:
                return result(error="gh: Not Found (HTTP 404)")
            return result(self.github)
        if args[0] == "buildkite-agent":
            if args[1:3] == ("artifact", "download"):
                if self.artifact_sources is not None:
                    matches = {
                        path: data
                        for path, data in self.artifact_sources[flag("--step")].items()
                        if fnmatch.fnmatchcase(path, args[3])
                    }
                    if not matches:
                        return result(error="No artifacts found")
                    for path, data in matches.items():
                        destination = Path(args[4]) / path.replace("\\", "/")
                        destination.parent.mkdir(parents=True, exist_ok=True)
                        destination.write_bytes(data)
                return result({})
            if args[1:3] == ("artifact", "upload"):
                for pattern in args[3].split(";"):
                    for path in sorted(Path().glob(pattern)):
                        if str(path) == self.fail_artifact:
                            return result(error="Simulated Buildkite artifact outage")
                        self.uploaded_artifacts[str(path)] = path.read_bytes()
                return result({})
            assert args[1:3] == ("meta-data", "get")
            return subprocess.CompletedProcess(args, 0, self.metadata[args[3]], "")
        if args[0] == "rustup":
            return result({})  # The local toolchain is already installed.
        if args[0] == "uv":
            return REAL_RUN([sys.executable, *args[5:]], **kwargs)
        if (
            args[0] == "git"
            and "push" in args
            and args[-1].startswith(tuple("0123456789abcdef"))
            and ":refs/tags/" in args[-1]
            and self.fail_tag
        ):
            return result(error="Simulated tag push rejection")
        return REAL_RUN(args, **kwargs)


@pytest.fixture
def cloud(release, monkeypatch):
    service = Cloud()
    monkeypatch.setattr(release.subprocess, "run", service.run)
    return service


def artifacts(release, product="compiler", version="1.2.3", dry_run=False):
    targets = [
        "aarch64-apple-darwin",
        "aarch64-unknown-linux-gnu",
        "aarch64-unknown-linux-musl",
        "x86_64-unknown-linux-gnu",
        "x86_64-unknown-linux-musl",
        "x86_64-pc-windows-msvc",
    ]
    names = ["pcbc"] if product == "compiler" else ["pcb", "pcb-launcher"]
    for target in targets:
        for binary in names:
            path = release.ASSETS / (
                f"{binary}-{target}" + (".exe" if "windows" in target else "")
            )
            path.write_bytes(f"{binary} {target} {version}\n".encode())
            REAL_RUN(["zstd", "-q", str(path), "-o", str(path) + ".zst"], check=True)
            path.with_name(path.name + ".sha256").write_text(
                f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n"
            )
    if product == "compiler":
        stdlib = release.ASSETS / "stdlib.tar.zst"
        stdlib.write_bytes(
            REAL_RUN(
                ["zstd", "-q", "-c"],
                input=b"source archive\n",
                capture_output=True,
                check=True,
            ).stdout
        )
        stdlib.with_name(stdlib.name + ".sha256").write_text(
            f"{hashlib.sha256(stdlib.read_bytes()).hexdigest()}  {stdlib.name}\n"
        )
    return {
        "product": product,
        "version": version,
        "tag": ("v" if product == "compiler" else "pcb/v") + version,
        "sha": git("rev-parse", "HEAD"),
        "dry_run": dry_run,
        "published_at": "2026-11-05T00:00:00+00:00",
    }


@pytest.mark.parametrize("product,count", [("compiler", 20), ("shim", 36)])
def test_publish_complete_release_then_tag_and_resume(
    release, cloud, product, count, monkeypatch
):
    config = artifacts(release, product)
    release.write_json(release.ROOT / "release.json", config)
    monkeypatch.setenv("BUILDKITE_PIPELINE_SLUG", "pcb-release")
    monkeypatch.setenv("BUILDKITE_BRANCH", "main")
    monkeypatch.setattr(sys, "argv", [str(SCRIPT), "publish"])
    monkeypatch.setattr(release, "credentials", lambda *_, **__: nullcontext())
    # Publication needs verified artifacts and an ancestor of main, not the build bundle.
    cloud.fail_tag = True
    with pytest.raises(subprocess.CalledProcessError):
        release.main()
    assert release.remote_tag(config["tag"]) is None
    marker = ("pcb/index/" if product == "compiler" else "pcb/shim-index/") + config[
        "tag"
    ]
    committed = json.loads(cloud.objects[marker])
    assert len(committed["assets"]) == count
    for name, digest in committed["assets"].items():
        assert (
            hashlib.sha256(cloud.objects[f"pcb/{config['tag']}/{name}"]).hexdigest()
            == digest
        )
    writes = cloud.writes
    cloud.fail_tag = False
    release.main()
    assert release.remote_tag(config["tag"]) == config["sha"]
    assert cloud.writes == writes  # Recovery did not overwrite any immutable S3 object.
    if product == "compiler":
        assert cloud.github["draft"] is False
        assert len(cloud.github["assets"]) == 20
    else:
        assert json.loads(cloud.objects["pcb/pcb-latest.json"])["sha"] == config["sha"]
        assert cloud.github is None
    release.main()  # A completely successful finalization is also idempotent.
    assert cloud.writes == writes


@pytest.mark.parametrize(
    "damage",
    [
        "missing-target",
        "bad-checksum",
        "bad-compressed",
        "denied",
        "upload-failure",
        "conflicting-object",
        "conflicting-tag",
    ],
)
def test_failure_never_exposes_incomplete_release(release, cloud, damage):
    config = artifacts(release)
    binary = release.ASSETS / "pcbc-aarch64-unknown-linux-musl"
    if damage == "missing-target":
        binary.unlink()
    elif damage == "bad-checksum":
        binary.write_bytes(b"different bytes\n")
    elif damage == "bad-compressed":
        binary.with_name(binary.name + ".zst").write_bytes(b"not zstd")
    elif damage == "denied":
        cloud.denied = True
    elif damage == "upload-failure":
        cloud.fail_key = f"pcb/{config['tag']}/{binary.name}"
    elif damage == "conflicting-object":
        cloud.objects[f"pcb/{config['tag']}/release-manifest.json"] = b"another release"
    else:
        git("tag", config["tag"])
        Path("source").write_text("different source\n")
        git("commit", "-am", "Different commit")
        config["sha"] = git("rev-parse", "HEAD")
        git("push", "origin", f"refs/tags/{config['tag']}", "main")
    with pytest.raises((ValueError, RuntimeError, subprocess.CalledProcessError)):
        release.publish(config)
    assert f"pcb/index/{config['tag']}" not in cloud.objects
    if damage != "conflicting-tag":
        assert release.remote_tag(config["tag"]) is None
    assert cloud.github is None


def test_github_upload_failure_leaves_draft_and_resumes(release, cloud):
    config = artifacts(release)
    cloud.fail_asset = True
    with pytest.raises(subprocess.CalledProcessError):
        release.publish(config)
    assert release.remote_tag(config["tag"]) == config["sha"]
    assert cloud.github["draft"] is True
    cloud.fail_asset = False
    release.publish(config)
    assert cloud.github["draft"] is False
    assert len(cloud.github["assets"]) == 20


def test_completed_github_asset_conflict_is_not_clobbered(release, cloud):
    config = artifacts(release)
    cloud.fail_asset = True
    with pytest.raises(subprocess.CalledProcessError):
        release.publish(config)
    cloud.fail_asset = False
    asset = cloud.github["assets"][0]
    asset.update(state="uploaded", size=42, digest="sha256:" + "0" * 64)
    with pytest.raises(ValueError, match="Conflicting GitHub asset"):
        release.publish(config)
    assert cloud.github["draft"] is True
    assert cloud.github["assets"] == [asset]


@pytest.mark.parametrize(
    "version,previous", [("1.2.3-rc.1", None), ("1.2.3", "1.2.10")]
)
def test_shim_prerelease_and_old_retry_do_not_change_latest(
    release, cloud, version, previous
):
    config = artifacts(release, "shim", version)
    if previous:
        cloud.objects["pcb/pcb-latest.json"] = json.dumps(
            {"version": previous, "tag": f"pcb/v{previous}"}
        ).encode()
    original = cloud.objects.get("pcb/pcb-latest.json")
    release.publish(config)
    assert cloud.objects.get("pcb/pcb-latest.json") == original
    assert release.remote_tag(config["tag"]) == config["sha"]


def test_dry_run_never_needs_writers(release, cloud):
    config = artifacts(release, dry_run=True)
    with release.credentials("publish", True):
        release.publish(config)
    assert cloud.objects == {}
    assert cloud.github is None
    assert release.remote_tag(config["tag"]) is None


@pytest.mark.parametrize("product", ["compiler", "shim", "nightly"])
@pytest.mark.parametrize("dry_run", [True, False])
def test_prepare_builds_requested_release_without_tagging(
    release, cloud, product, dry_run, monkeypatch
):
    Path("Cargo.toml").write_text(
        '[workspace]\nmembers = ["crates/*"]\nresolver = "2"\n[workspace.package]\nversion = "1.2.3-rc.1"\n[profile.dist]\ninherits = "release"\n'
    )
    for name in ("pcbc", "pcb", "pcb-launcher"):
        directory = Path("crates") / name
        (directory / "src").mkdir(parents=True)
        (directory / "Cargo.toml").write_text(
            f'[package]\nname = "{name}"\n'
            + ('version = "0.1.0"\n' if name == "pcb" else "version.workspace = true\n")
            + 'edition = "2021"\n'
        )
        (directory / "src/main.rs").write_text(
            'fn main() { println!("{}", env!("CARGO_PKG_VERSION")); }\n'
        )
    Path("lib/std").mkdir(parents=True)
    Path("lib/std/test.zen").write_text("# Standard library\n")
    Path("CHANGELOG.md").write_text(
        "## [Unreleased]\n\n### Added\n\n- Feature\n\n## [1.2.2]\n\n[Unreleased]: https://github.com/diodeinc/pcb/compare/v1.2.2...HEAD\n"
    )
    Path("bin").mkdir()
    shutil.copyfile(SCRIPT.parent / "changelog.py", "bin/changelog.py")
    REAL_RUN(["cargo", "generate-lockfile"], check=True)
    git("add", ".")
    git("commit", "-m", "Cargo fixture")
    git("push", "origin", "main")
    base = git("rev-parse", "HEAD")
    cloud.metadata = {
        "release-product": product,
        "release-version": "1.2.3",
        "release-dry-run": str(dry_run).lower(),
    }
    nightly = product == "nightly"
    if nightly:
        monkeypatch.setenv("PCB_RELEASE_CHANNEL", "nightly")
        monkeypatch.setenv("PCB_RELEASE_DRY_RUN", str(dry_run).lower())
    if not dry_run:
        # Only the local Git remote is writable; no AWS/GitHub credentials are acquired.
        monkeypatch.setattr(release, "credentials", lambda *_, **__: nullcontext())
    if not dry_run and not nightly:
        cloud.fail_artifact = str(
            release.ASSETS / "stdlib.tar.zst.sha256"
            if product == "compiler"
            else release.ROOT / "source.bundle"
        )
        with pytest.raises(subprocess.CalledProcessError):
            release.prepare()
        assert git("ls-remote", "origin", "refs/heads/main").split()[0] == base
        assert (
            release.remote_tag(("v" if product == "compiler" else "pcb/v") + "1.2.3")
            is None
        )
        # Buildkite retries prepare from the original build checkout.
        git("reset", "--hard", base)
        cloud.fail_artifact = None
    release.prepare()
    config = release.load_release()
    assert (config["sha"] == base) is nightly
    assert config.get("version") == (None if nightly else "1.2.3")
    assert git("ls-remote", "origin", "refs/heads/main").split()[0] == (
        base if dry_run or nightly else config["sha"]
    )
    for name in ("release.json",) if nightly else ("release.json", "source.bundle"):
        path = release.ROOT / name
        assert cloud.uploaded_artifacts[str(path)] == path.read_bytes()
    assert release.remote_tag(config["tag"]) is None
    metadata = json.loads(
        REAL_RUN(
            ["cargo", "metadata", "--locked", "--no-deps", "--format-version=1"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    assert next(
        item["version"]
        for item in metadata["packages"]
        if item["name"] == ("pcb" if product == "shim" else "pcbc")
    ) == ("1.2.3-rc.1" if nightly else "1.2.3")
    if not nightly:
        git("bundle", "verify", str(release.ROOT / "source.bundle"))
    if product == "compiler":
        assert "## [1.2.3] - " in Path("CHANGELOG.md").read_text()
    else:
        assert "## [1.2.3] - " not in Path("CHANGELOG.md").read_text()
    if product != "shim":
        assert (release.ASSETS / "stdlib.tar.zst").is_file()
        for path in release.ASSETS.iterdir():
            assert cloud.uploaded_artifacts[str(path)] == path.read_bytes()
    assert cloud.objects == {}
    target_dir = Path("build-cache").resolve()
    monkeypatch.setenv("CARGO_TARGET_DIR", str(target_dir))
    target = REAL_RUN(
        ["rustc", "--print", "host-tuple"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    release.build(target)
    exe = ".exe" if "windows" in target else ""
    for name in ["pcb", "pcb-launcher"] if product == "shim" else ["pcbc"]:
        path = release.ASSETS / f"{name}-{target}{exe}"
        assert REAL_RUN(
            [str(target_dir / target / "dist" / (name + exe))],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip() == (
            "1.2.3-rc.1" if nightly or name == "pcb-launcher" else "1.2.3"
        )
        assert (
            REAL_RUN(
                ["zstd", "-q", "-d", "-c", str(path) + ".zst"],
                check=True,
                capture_output=True,
            ).stdout
            == path.read_bytes()
        )
    if nightly:
        cloud.objects["pcb/nightly/latest.json"] = json.dumps({"sha": base}).encode()
        assert release.prepare() is dry_run
        cloud.denied = True
        if dry_run:
            assert release.prepare() is True
        else:
            with pytest.raises(subprocess.CalledProcessError):
                release.prepare()


@pytest.mark.parametrize(
    "mode", ["publish", "dry-run", "upload-failure", "older-retry", "windows-artifacts"]
)
def test_nightly_publication_preserves_download_contract(
    release, cloud, mode, monkeypatch
):
    config = artifacts(release, dry_run=mode == "dry-run")
    config.update(channel="nightly", date="2026-11-05")
    config["tag"] = f"nightly/2026-11-05/{config['sha']}"
    del config["version"]
    release.write_json(release.ROOT / "release.json", config)
    monkeypatch.setenv("BUILDKITE_PIPELINE_SLUG", "pcb-release")
    monkeypatch.setenv("BUILDKITE_BRANCH", "main")
    monkeypatch.setattr(sys, "argv", [str(SCRIPT), "publish"])
    monkeypatch.setattr(release, "credentials", lambda *_, **__: nullcontext())
    if mode == "windows-artifacts":
        cloud.artifact_sources = {}
        for path in release.ROOT.rglob("*"):
            if not path.is_file():
                continue
            if "windows" in path.name:
                step = "release-windows"
                remote_path = str(path).replace("/", "\\")
            else:
                remote_path = path.as_posix()
                if "apple" in path.name:
                    step = "release-macos"
                elif "x86_64" in path.name:
                    step = "release-linux-amd64"
                elif "aarch64" in path.name:
                    step = "release-linux-arm64"
                else:
                    step = "prepare-release"
            cloud.artifact_sources.setdefault(step, {})[remote_path] = path.read_bytes()
        shutil.rmtree(release.ROOT)
    pointer = "pcb/nightly/latest.json"
    if mode == "upload-failure":
        cloud.fail_key = f"pcb/{config['tag']}/pcbc-x86_64-pc-windows-msvc.exe"
        with pytest.raises(subprocess.CalledProcessError):
            release.main()
        assert pointer not in cloud.objects
        cloud.fail_key = None
    release.main()
    assert cloud.github is None
    assert git("ls-remote", "--tags", "origin") == ""
    if mode == "dry-run":
        assert cloud.objects == {}
        return
    latest = json.loads(cloud.objects[pointer])
    assert latest == {
        "channel": "nightly",
        "date": "2026-11-05",
        "sha": config["sha"],
        "tag": f"nightly/2026-11-05/{config['sha']}",
        "base_url": f"{release.BASE_URL}/nightly/2026-11-05/{config['sha']}",
        "published_at": latest["published_at"],
    }
    for path in release.ASSETS.iterdir():
        assert cloud.objects[f"pcb/{config['tag']}/{path.name}"] == path.read_bytes()
    writes = cloud.writes
    release.main()
    assert cloud.writes == writes
    if mode == "older-retry":
        Path("source").write_text("newer nightly\n")
        git("commit", "-am", "Newer nightly")
        git("push", "origin", "main")
        newer = {**latest, "sha": git("rev-parse", "HEAD")}
        cloud.objects[pointer] = json.dumps(newer).encode()
        with pytest.raises(subprocess.CalledProcessError):
            release.main()
        assert json.loads(cloud.objects[pointer]) == newer
