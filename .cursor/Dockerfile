# Cloud Agent base image for the pcb / Zener toolchain.
#
# Debian 12 (bookworm) is required because the Diode KiCad 10 package that the
# workspace tests and `pcb layout` depend on is built for Debian 12 (it depends
# on libpython3.11, libocct-*-7.6, libwxbase3.2, etc. that only resolve there).
# This mirrors CI, which runs the workspace inside a Debian-based KiCad image.
#
# Repository-dependent work (pinned Rust toolchain, cargo fetch/build, uv sync)
# lives in the `install` script from .cursor/environment.json, not here.
FROM debian:12-slim

ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin

# --- Base system packages ----------------------------------------------------
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        jq \
        xz-utils \
        build-essential \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# --- Diode KiCad 10 (native Debian package + Python bindings) ----------------
# Installed via apt so the KiCad dependency graph is resolved automatically.
RUN set -eux; \
    metadata="$(curl -fsSL https://kicad-mirror.api.diode.computer/packages/kicad/diode/10.0/latest.json)"; \
    deb_url="$(jq -er .deb_url <<<"$metadata")"; \
    sha256="$(jq -er .sha256 <<<"$metadata")"; \
    tmp="$(mktemp -d)"; \
    curl -fsSL "$deb_url" -o "$tmp/kicad.deb"; \
    printf '%s  %s\n' "$sha256" "$tmp/kicad.deb" | sha256sum --check --strict; \
    apt-get update; \
    apt-get install -y --no-install-recommends "$tmp/kicad.deb"; \
    rm -rf "$tmp" /var/lib/apt/lists/*; \
    kicad-cli --version; \
    /usr/bin/python3 -c 'import pcbnew'

# --- Rust toolchain (rustup; pinned channel resolved from rust-toolchain.toml
#     by the install script after checkout) -----------------------------------
RUN set -eux; \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain stable --component clippy rustfmt; \
    chmod -R a+w "$RUSTUP_HOME" "$CARGO_HOME"; \
    rustup --version; \
    cargo --version

# --- cargo-nextest (official prebuilt binary; stay on the 0.9 series) --------
RUN set -eux; \
    curl -LsSf https://get.nexte.st/0.9/linux | tar zxf - -C /usr/local/cargo/bin; \
    cargo nextest --version

# --- uv (standalone installer) ----------------------------------------------
RUN set -eux; \
    curl -LsSf https://astral.sh/uv/install.sh \
        | env UV_INSTALL_DIR=/usr/local/bin UV_UNMANAGED_INSTALL=/usr/local/bin sh; \
    uv --version
