#!/usr/bin/env -S uv run --script
"""Changelog management script."""

import sys
from datetime import date
from pathlib import Path

import click

CHANGELOG_PATH = Path("CHANGELOG.md")
REPO_URL = "https://github.com/diodeinc/pcb"
CATEGORIES = ["Added", "Changed", "Deprecated", "Removed", "Fixed", "Security"]


def read_changelog() -> list[str]:
    if not CHANGELOG_PATH.exists():
        click.echo(f"Error: {CHANGELOG_PATH} not found", err=True)
        sys.exit(1)
    return CHANGELOG_PATH.read_text().splitlines()


def write_changelog(lines: list[str]) -> None:
    CHANGELOG_PATH.write_text("\n".join(lines) + "\n")


@click.group()
def cli() -> None:
    """Changelog management for releases."""


@cli.command()
@click.argument("category", type=click.Choice(CATEGORIES, case_sensitive=True))
@click.argument("entry")
def add(category: str, entry: str) -> None:
    """Add an entry to the Unreleased section."""
    lines = read_changelog()
    result = []

    in_unreleased = False
    added = False
    skip_blank = False

    for line in lines:
        if skip_blank:
            skip_blank = False
            result.append(line)  # Keep the blank line after header
            result.append(f"- {entry}")
            continue

        if line == "## [Unreleased]":
            in_unreleased = True
            result.append(line)
            continue

        if in_unreleased and line.startswith("## [") and "Unreleased" not in line:
            # Exiting unreleased section without finding category - add it
            if not added:
                result.append(f"### {category}")
                result.append("")
                result.append(f"- {entry}")
                result.append("")
            in_unreleased = False

        if in_unreleased and line == f"### {category}":
            result.append(line)
            added = True
            skip_blank = True
            continue

        result.append(line)

    write_changelog(result)
    click.echo(f"Added to CHANGELOG.md: [{category}] {entry}")


@cli.command()
@click.argument("version")
def release(version: str) -> None:
    """Convert Unreleased section to a versioned release."""
    lines = read_changelog()
    result = []

    display_version = version.lstrip("v")
    today = date.today().isoformat()
    prev_version = None

    for line in lines:
        if line == "## [Unreleased]":
            result.append(line)
            result.append("")
            result.append(f"## [{display_version}] - {today}")
            continue

        if line.startswith("[Unreleased]:") and "...HEAD" in line:
            # Extract previous version: [Unreleased]: .../compare/v0.3.18...HEAD
            prev_version = line.split("/compare/")[1].split("...")[0]
            result.append(line.replace(f"{prev_version}...HEAD", f"{version}...HEAD"))
            result.append(f"[{display_version}]: {REPO_URL}/compare/{prev_version}...{version}")
            continue

        result.append(line)

    write_changelog(result)
    click.echo(f"Updated CHANGELOG.md for {version}")


if __name__ == "__main__":
    cli()
