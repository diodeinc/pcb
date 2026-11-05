#!/usr/bin/env python3
"""Protected Buildkite release entry point. Requires Python 3.11 and preinstalled tools."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
import time
import urllib.request
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from typing import IO

import tomllib

ROOT = Path("target/release-pipeline")
ASSETS = ROOT / "artifacts"
TARGETS = (
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "x86_64-pc-windows-msvc",
)
REPO = "diodeinc/pcb"
BUCKET = "diode-pcb-releases"
BASE_URL = "https://pcb.api.diode.computer/pcb"
VERSION = re.compile(
    r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-((?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
    r"(?:\.(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*))*))?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
)


def run(*args: str, input: str | None = None) -> str:
    return subprocess.run(
        args, input=input, text=True, stdout=subprocess.PIPE, check=True
    ).stdout.strip()


def hash_stream(source: IO[bytes]) -> str:
    digest = hashlib.sha256()
    for chunk in iter(lambda: source.read(1024 * 1024), b""):
        digest.update(chunk)
    return digest.hexdigest()


def sha256(path: Path) -> str:
    with path.open("rb") as source:
        return hash_stream(source)


def write_json(path: Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, sort_keys=True) + "\n")


def binaries(product: str) -> tuple[str, ...]:
    return ("pcbc",) if product == "compiler" else ("pcb", "pcb-launcher")


def version_key(version: str) -> tuple:
    match = VERSION.fullmatch(version)
    if match is None:
        raise ValueError(f"Invalid SemVer: {version}")
    prerelease = match.group(4)
    parts = tuple(
        (0, int(part)) if part.isdigit() else (1, part)
        for part in (prerelease or "").split(".")
    )
    return (*map(int, match.group(1, 2, 3)), prerelease is None, parts)


def asset_names(product: str) -> set[str]:
    names = set()
    for target in TARGETS:
        for binary in binaries(product):
            name = f"{binary}-{target}" + (".exe" if "windows" in target else "")
            names.update((name, name + ".zst", name + ".sha256"))
    if product == "compiler":
        names.update(("stdlib.tar.zst", "stdlib.tar.zst.sha256"))
    return names


def load_release() -> dict:
    release = json.loads((ROOT / "release.json").read_text())
    if (
        release["product"] not in ("compiler", "shim")
        or not VERSION.fullmatch(release["version"])
        or not re.fullmatch(r"[0-9a-f]{40}", release["sha"])
        or not isinstance(release["dry_run"], bool)
    ):
        raise ValueError("Invalid release metadata")
    prefix = "v" if release["product"] == "compiler" else "pcb/v"
    if release["tag"] != prefix + release["version"]:
        raise ValueError("Release tag does not match product/version")
    return release


@contextmanager
def credentials(stage: str, dry_run: bool):
    if dry_run:
        yield
        return
    with tempfile.TemporaryDirectory() as directory:
        token_file = Path(directory) / "oidc"
        token_file.write_text(
            run(
                "buildkite-agent",
                "oidc",
                "request-token",
                "--audience",
                "sts.amazonaws.com",
                "--claim",
                "cluster_id",
            )
        )
        token_file.chmod(0o600)
        previous = os.environ.copy()
        try:
            os.environ.update(
                AWS_ROLE_ARN=f"arn:aws:iam::689688455535:role/buildkite-pcb-release-{stage}",
                AWS_WEB_IDENTITY_TOKEN_FILE=str(token_file.resolve()),
                AWS_DEFAULT_REGION="us-west-2",
                AWS_REGION="us-west-2",
            )
            for key in (
                "AWS_ACCESS_KEY_ID",
                "AWS_SECRET_ACCESS_KEY",
                "AWS_SESSION_TOKEN",
                "AWS_PROFILE",
            ):
                os.environ.pop(key, None)
            app = json.loads(
                run(
                    "aws",
                    "secretsmanager",
                    "get-secret-value",
                    "--secret-id",
                    "buildkite/pcb-release/github-app",
                    "--query",
                    "SecretString",
                    "--output",
                    "text",
                )
            )
            key_file = Path(directory) / "app.pem"
            key_file.write_text(app["private_key"])
            key_file.chmod(0o600)

            def encode(value: dict) -> str:
                return (
                    base64.urlsafe_b64encode(json.dumps(value).encode())
                    .decode()
                    .rstrip("=")
                )

            now = int(time.time())
            payload = (
                encode({"alg": "RS256", "typ": "JWT"})
                + "."
                + encode({"iat": now - 60, "exp": now + 540, "iss": str(app["app_id"])})
            )
            signature = subprocess.run(
                ["openssl", "dgst", "-sha256", "-sign", str(key_file)],
                input=payload.encode(),
                stdout=subprocess.PIPE,
                check=True,
            ).stdout
            jwt = (
                payload + "." + base64.urlsafe_b64encode(signature).decode().rstrip("=")
            )
            permissions = {"contents": "write"}
            if stage == "publish":
                # main can gain workflow changes during a build. GitHub requires
                # this permission to tag/release the older, already-built commit.
                permissions["workflows"] = "write"
            request = urllib.request.Request(
                f"https://api.github.com/app/installations/{int(app['installation_id'])}/access_tokens",
                data=json.dumps(
                    {"repositories": ["pcb"], "permissions": permissions}
                ).encode(),
                headers={
                    "Authorization": f"Bearer {jwt}",
                    "Accept": "application/vnd.github+json",
                },
                method="POST",
            )
            with urllib.request.urlopen(request, timeout=30) as response:
                os.environ["GH_TOKEN"] = json.load(response)["token"]
            run("buildkite-agent", "redactor", "add", input=os.environ["GH_TOKEN"])
            yield
        finally:
            os.environ.clear()
            os.environ.update(previous)


def git_push(ref: str) -> None:
    run(
        "git",
        "-c",
        "credential.helper=",
        "-c",
        "credential.helper=!gh auth git-credential",
        "push",
        "origin",
        ref,
    )


def remote_tag(tag: str) -> str | None:
    refs = run(
        "git", "ls-remote", "origin", f"refs/tags/{tag}", f"refs/tags/{tag}^{{}}"
    )
    entries = [line.split() for line in refs.splitlines()]
    return next(
        (sha for sha, ref in entries if ref.endswith("^{}")),
        entries[0][0] if entries else None,
    )


def checksum_file(path: Path) -> None:
    path.with_name(path.name + ".sha256").write_text(f"{sha256(path)}  {path.name}\n")


def prepare() -> None:
    product = run("buildkite-agent", "meta-data", "get", "release-product")
    version = run("buildkite-agent", "meta-data", "get", "release-version")
    dry_run = run("buildkite-agent", "meta-data", "get", "release-dry-run")
    if (
        product not in ("compiler", "shim")
        or not VERSION.fullmatch(version)
        or dry_run not in ("true", "false")
    ):
        raise ValueError("Select compiler/shim, a valid SemVer and true/false dry-run")
    tag = ("v" if product == "compiler" else "pcb/v") + version
    if run("git", "status", "--porcelain", "--untracked-files=no"):
        raise ValueError("Prepare requires a clean tracked checkout")
    run("git", "fetch", "--no-tags", "origin", "main")
    base = run("git", "rev-parse", "HEAD")
    if base != run("git", "rev-parse", "FETCH_HEAD"):
        raise ValueError("Start a new release build from current main")
    if remote_tag(tag):
        raise ValueError(
            f"Tag {tag} already exists; retry the original publish job instead"
        )
    manifest = Path("Cargo.toml" if product == "compiler" else "crates/pcb/Cargo.toml")
    text = manifest.read_text()
    current = tomllib.loads(text)["workspace" if product == "compiler" else "package"]
    if product == "compiler":
        current = current["package"]
    if version_key(version) <= version_key(current["version"]):
        raise ValueError(
            "Version must increase; resume a failed release using its original build"
        )
    manifest.write_text(
        re.sub(
            r'^version = "[^"]+"$',
            f'version = "{version}"',
            text,
            count=1,
            flags=re.MULTILINE,
        )
    )
    if product == "compiler":
        run(
            "uv",
            "run",
            "--locked",
            "--no-default-groups",
            "python",
            "bin/changelog.py",
            "release",
            tag,
        )
    run("cargo", "update", "--workspace")
    files = [str(manifest), "Cargo.lock"] + (
        ["CHANGELOG.md"] if product == "compiler" else []
    )
    run("git", "add", "--", *files)
    run(
        "git",
        "-c",
        "user.name=Diode Robot",
        "-c",
        "user.email=info@diode.run",
        "commit",
        "-m",
        f"Release {tag}",
    )
    sha = run("git", "rev-parse", "HEAD")
    release = {
        "product": product,
        "version": version,
        "tag": tag,
        "sha": sha,
        "dry_run": dry_run == "true",
        "published_at": datetime.now(timezone.utc).isoformat(),
    }
    write_json(ROOT / "release.json", release)
    run("git", "update-ref", "refs/pcb-release/source", sha)
    # Every job checks out the build's original main commit. Transfer only the bump delta.
    run(
        "git",
        "bundle",
        "create",
        str(ROOT / "source.bundle"),
        "refs/pcb-release/source",
        f"^{base}",
    )
    if product == "compiler":
        ASSETS.mkdir(parents=True, exist_ok=True)
        tar = ROOT / "stdlib.tar"
        run("git", "archive", "--format=tar", f"--output={tar}", f"{sha}:lib/std")
        run("zstd", "-q", "-f", "-3", str(tar), "-o", str(ASSETS / "stdlib.tar.zst"))
        tar.unlink()
        checksum_file(ASSETS / "stdlib.tar.zst")
    # Persist recovery artifacts before making the version bump visible on main.
    run(
        "buildkite-agent",
        "artifact",
        "upload",
        f"{ROOT}/release.json;{ROOT}/source.bundle"
        + (f";{ASSETS}/*" if product == "compiler" else ""),
    )
    with credentials("prepare", release["dry_run"]):
        if not release["dry_run"]:
            git_push(f"{sha}:refs/heads/main")
    print(f"Prepared {tag} from {sha}; no tag created")


def download_artifacts(pattern: Path, step: str) -> None:
    run("buildkite-agent", "artifact", "download", str(pattern), ".", "--step", step)


def build(target: str) -> None:
    download_artifacts(ROOT / "release.json", "prepare-release")
    release = load_release()
    if "windows" in target:
        run("git", "config", "--global", "core.longpaths", "true")
    download_artifacts(ROOT / "source.bundle", "prepare-release")
    run("git", "fetch", str(ROOT / "source.bundle"), "refs/pcb-release/source")
    if run("git", "rev-parse", "FETCH_HEAD") != release["sha"]:
        raise ValueError("Source bundle does not match release commit")
    run("git", "checkout", "--detach", release["sha"])
    nix = os.environ.get("PCB_RELEASE_NIX") == "1"
    if not nix:
        run("rustup", "set", "profile", "minimal")
        run("rustup", "target", "add", target)
    host = run("rustc", "-vV").split("host: ")[1].splitlines()[0]
    if (
        target.split("-")[0] != host.split("-")[0]
        or ("windows" in target) != ("windows" in host)
        or ("apple" in target) != ("apple" in host)
    ):
        raise ValueError(f"Expected native builder for {target}, got {host}")
    os.environ["CARGO_INCREMENTAL"] = "0"
    os.environ["RUSTFLAGS"] = ""
    if target.endswith("linux-musl"):
        os.environ["CFLAGS"] = (
            "-Du_int8_t=uint8_t -Du_int16_t=uint16_t -Du_int32_t=uint32_t -Du_int64_t=uint64_t"
        )
        if not nix:
            os.environ.update(CC="musl-gcc", RUSTFLAGS="-C linker=musl-gcc")
    packages = [
        item
        for binary in binaries(release["product"])
        for item in ("-p", binary, "--bin", binary)
    ]
    zig = nix and "linux" in target
    # Zig avoids Nix store interpreters/RPATHs and pins GNU's minimum glibc ABI.
    build_target = target + ".2.28" if zig and target.endswith("linux-gnu") else target
    run(
        "cargo",
        "zigbuild" if zig else "build",
        "--locked",
        "--profile",
        "dist",
        "--target",
        build_target,
        *packages,
    )
    ASSETS.mkdir(parents=True, exist_ok=True)
    for binary in binaries(release["product"]):
        exe = ".exe" if "windows" in target else ""
        source = (
            Path(os.environ.get("CARGO_TARGET_DIR", "target"))
            / target
            / "dist"
            / (binary + exe)
        )
        artifact = ASSETS / f"{binary}-{target}{exe}"
        shutil.copyfile(source, artifact)
        run("zstd", "-q", "-f", "-3", str(artifact), "-o", str(artifact) + ".zst")
        checksum_file(artifact)


def head(key: str) -> dict | None:
    # Prefix-scoped ListBucket does not grant HeadObject's missing-object 404.
    listing = json.loads(
        run(
            "aws",
            "s3api",
            "list-objects-v2",
            "--bucket",
            BUCKET,
            "--prefix",
            key,
            "--max-keys",
            "1",
        )
    )
    if not any(item["Key"] == key for item in listing.get("Contents", [])):
        return None
    return json.loads(
        run(
            "aws",
            "s3api",
            "head-object",
            "--bucket",
            BUCKET,
            "--key",
            key,
            "--checksum-mode",
            "ENABLED",
        )
    )


def put_immutable(key: str, path: Path) -> None:
    digest = base64.b64encode(bytes.fromhex(sha256(path))).decode()
    existing = head(key)
    if existing is not None:
        if (
            existing.get("ChecksumSHA256") != digest
            or existing["ContentLength"] != path.stat().st_size
        ):
            raise ValueError(f"Refusing to overwrite conflicting release object {key}")
        return
    run(
        "aws",
        "s3api",
        "put-object",
        "--bucket",
        BUCKET,
        "--key",
        key,
        "--body",
        str(path),
        "--if-none-match",
        "*",
        "--checksum-algorithm",
        "SHA256",
        "--checksum-sha256",
        digest,
        "--cache-control",
        "public, max-age=31536000, immutable",
    )
    saved = head(key)
    if saved is None or saved.get("ChecksumSHA256") != digest:
        raise ValueError(f"Uploaded object failed checksum verification: {key}")


def publish_shim_pointer(release: dict) -> None:
    key = "pcb/pcb-latest.json"
    existing = head(key)
    condition = ["--if-none-match", "*"]
    if existing is not None:
        old = ROOT / "previous-latest.json"
        run(
            "aws",
            "s3api",
            "get-object",
            "--bucket",
            BUCKET,
            "--key",
            key,
            "--if-match",
            existing["ETag"],
            str(old),
        )
        previous = json.loads(old.read_text())
        current = VERSION.fullmatch(previous["version"])
        desired = VERSION.fullmatch(release["version"])
        assert current is not None and desired is not None
        if tuple(map(int, current.group(1, 2, 3))) > tuple(
            map(int, desired.group(1, 2, 3))
        ):
            print("A newer shim is already latest; leaving its pointer unchanged")
            return
        if previous["tag"] == release["tag"]:
            if previous.get("sha") != release["sha"]:
                raise ValueError("Latest shim pointer conflicts with this release")
            return
        condition = ["--if-match", existing["ETag"]]
    pointer = ROOT / "latest.json"
    write_json(pointer, {**release, "base_url": f"{BASE_URL}/{release['tag']}"})
    run(
        "aws",
        "s3api",
        "put-object",
        "--bucket",
        BUCKET,
        "--key",
        key,
        "--body",
        str(pointer),
        "--content-type",
        "application/json",
        "--cache-control",
        "public, max-age=60",
        *condition,
    )


def github_release(release: dict, assets: dict[str, str]) -> None:
    endpoint = f"repos/{REPO}/releases"
    # The by-tag endpoint misses drafts left by interrupted publication.
    pages = json.loads(
        run("gh", "api", f"{endpoint}?per_page=100", "--paginate", "--slurp")
    )
    matches = [
        item for page in pages for item in page if item["tag_name"] == release["tag"]
    ]
    if len(matches) > 1:
        raise ValueError(
            "Multiple GitHub releases use this tag; reconcile them before retrying"
        )
    existing = matches[0] if matches else None
    if existing is None:
        existing = json.loads(
            run(
                "gh",
                "api",
                endpoint,
                "--method",
                "POST",
                "--input",
                "-",
                input=json.dumps(
                    {
                        "tag_name": release["tag"],
                        "target_commitish": release["sha"],
                        "name": release["version"],
                        "body": f"Release {release['tag']} from {release['sha']}",
                        "draft": True,
                        "prerelease": "-" in release["version"].split("+")[0],
                    }
                ),
            )
        )
    uploaded = {asset["name"]: asset for asset in existing["assets"]}
    if set(uploaded) - assets.keys():
        raise ValueError("GitHub release contains unexpected assets")
    for name, digest in assets.items():
        asset = uploaded.get(name)
        if (
            asset
            and existing["draft"]
            and asset["state"] == "starter"
            and asset["size"] == 0
        ):
            # GitHub 502s can leave empty starter assets; only these are disposable.
            run("gh", "api", f"{endpoint}/assets/{asset['id']}", "--method", "DELETE")
            del uploaded[name]
        if name in uploaded:
            if (
                uploaded[name]["state"] != "uploaded"
                or uploaded[name].get("digest") != f"sha256:{digest}"
            ):
                raise ValueError(
                    f"Conflicting GitHub asset {name}; never clobber published artifacts"
                )
        elif existing["draft"]:
            run(
                "gh",
                "release",
                "upload",
                release["tag"],
                str(ASSETS / name),
                "--repo",
                REPO,
            )
        else:
            raise ValueError(f"Published GitHub release is missing {name}")
    verified = json.loads(run("gh", "api", f"{endpoint}/{existing['id']}"))
    actual = {asset["name"]: asset.get("digest") for asset in verified["assets"]}
    if actual != {name: f"sha256:{digest}" for name, digest in assets.items()} or any(
        asset["state"] != "uploaded" for asset in verified["assets"]
    ):
        raise ValueError("GitHub draft assets failed verification")
    if verified["draft"]:
        run(
            "gh",
            "api",
            f"{endpoint}/{existing['id']}",
            "--method",
            "PATCH",
            "--input",
            "-",
            input=json.dumps({"draft": False, "make_latest": "legacy"}),
        )


def publish(release: dict) -> None:
    expected = asset_names(release["product"])
    paths = list(ASSETS.iterdir())
    if {path.name for path in paths} != expected or any(
        not path.is_file() or path.is_symlink() for path in paths
    ):
        raise ValueError(
            "Release artifacts must contain exactly all six targets and required stdlib"
        )
    for path in paths:
        if path.name.endswith(".sha256"):
            original = path.with_suffix("")
            if path.read_text() != f"{sha256(original)}  {original.name}\n":
                raise ValueError(f"Invalid checksum file {path.name}")
        if path.name.endswith(".zst"):
            with subprocess.Popen(
                ["zstd", "-q", "-d", "-c", str(path)], stdout=subprocess.PIPE
            ) as process:
                assert process.stdout is not None
                digest = hash_stream(process.stdout)
                if process.wait() != 0:
                    raise ValueError(f"Invalid compressed artifact {path.name}")
            if path.name != "stdlib.tar.zst" and digest != sha256(path.with_suffix("")):
                raise ValueError(
                    f"Compressed artifact differs from original: {path.name}"
                )
    assets = {path.name: sha256(path) for path in sorted(paths)}
    manifest = ROOT / "release-manifest.json"
    write_json(manifest, {**release, "assets": assets})
    if release["dry_run"]:
        print(f"Dry run verified {len(assets)} artifacts; no GitHub/S3 writes or tag")
        return
    tag_sha = remote_tag(release["tag"])
    if tag_sha is not None and tag_sha != release["sha"]:
        raise ValueError("Existing tag points at another commit")
    run("git", "fetch", "--no-tags", "origin", "main")
    run("git", "merge-base", "--is-ancestor", release["sha"], "FETCH_HEAD")
    prefix = f"pcb/{release['tag']}"
    put_immutable(f"{prefix}/release-manifest.json", manifest)
    for path in sorted(paths):
        put_immutable(f"{prefix}/{path.name}", path)
    marker = (
        f"pcb/index/{release['tag']}"
        if release["product"] == "compiler"
        else f"pcb/shim-index/{release['tag']}"
    )
    put_immutable(marker, manifest)
    if release["product"] == "shim" and "-" not in release["version"].split("+")[0]:
        publish_shim_pointer(release)
    # S3 completion must precede the tag.
    if tag_sha is None:
        git_push(f"{release['sha']}:refs/tags/{release['tag']}")
    if release["product"] == "compiler":
        github_release(release, assets)
    print(f"Published {release['tag']} from {release['sha']}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("prepare")
    commands.add_parser("build").add_argument("target", choices=TARGETS)
    commands.add_parser("publish")
    args = parser.parse_args()
    if (
        os.environ.get("BUILDKITE_PIPELINE_SLUG") != "pcb-release"
        or os.environ.get("BUILDKITE_BRANCH") != "main"
    ):
        parser.error("Only the protected pcb-release pipeline on main may run releases")
    if args.command == "prepare":
        prepare()
    elif args.command == "build":
        build(args.target)
    else:
        download_artifacts(ROOT / "release.json", "prepare-release")
        release = load_release()
        if release["product"] == "compiler":
            download_artifacts(ASSETS / "*", "prepare-release")
        for step in (
            "release-linux-amd64",
            "release-linux-arm64",
            "release-macos",
            "release-windows",
        ):
            download_artifacts(ASSETS / "*", step)
        with credentials("publish", release["dry_run"]):
            publish(release)


if __name__ == "__main__":
    main()
