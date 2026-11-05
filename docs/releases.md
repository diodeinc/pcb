# PCB releases

Start a manual `pcb-release` Buildkite build from current `main`. Select
`compiler` or `shim`, enter an increasing SemVer, and leave **Dry run** on to
build and verify without writer credentials or GitHub/S3 writes.

The pipeline bumps the version on main, builds six native targets from that
commit, verifies and uploads immutable S3 artifacts, writes the completion
marker/latest pointer, then pushes the tag. Compiler releases also publish a
verified GitHub Release; shims stay S3-only. Nightly is unchanged.

S3 and GitHub cannot commit atomically: complete downloads can precede the tag.
Direct version URLs may be accessible during upload, but version discovery
waits for completion. Conflicting objects, tags and completed assets are never
overwritten; older shim retries cannot downgrade latest.

## Recovery

Retry failed build/publish jobs in the **same build**, retaining its artifacts.
Do not retry prepare once its bump reached main or start another release with
the same version. Publish resumes matching S3 objects and GitHub draft assets.
There are no automatic retries. Main may advance while builds run; the release
commit must remain its ancestor.

## Activation

The pipeline stays archived until the companion Diode infrastructure is ready.
Follow `infrastructure/terraform/environments/buildkite/pcb-release.md` for
isolated workers, signed hooks, AWS roles and GitHub App setup. Run the full
signed six-target dry run and denied-access checks before enabling publication.
This change removes the four old stable Actions workflows: coordinate merging
with activation to avoid a stable-release outage. Keep nightly and its AWS role.
