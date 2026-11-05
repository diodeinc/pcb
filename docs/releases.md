# PCB releases

Start a manual `pcb-release` Buildkite build from current `main`. Select
`compiler` or `shim`, enter an increasing SemVer, and leave **Dry run** on to
build and verify without writer credentials or GitHub/S3 writes.

The pipeline bumps the version on main, builds six native targets from that
commit, verifies and uploads immutable S3 artifacts, writes the completion
marker/latest pointer, then pushes the tag. Compiler releases also publish a
verified GitHub Release; shims stay S3-only.

## Nightly

`PCB_RELEASE_CHANNEL=nightly` runs the same six-target pipeline from `main`,
without version bumps, GitHub credentials or tags. The **07:17 UTC** schedule skips
unchanged SHAs; publication advances `pcb/nightly/latest.json` only after every
artifact is verified. Older retries cannot downgrade it. Add
`PCB_RELEASE_DRY_RUN=true` for a credential-free manual dry run, even on an
unchanged SHA.

S3 and GitHub cannot commit atomically: complete downloads can precede the tag.
Direct version URLs may be accessible during upload, but version discovery
waits for completion. Conflicting objects, tags and completed assets are never
overwritten; older shim retries cannot downgrade latest.

## Recovery

Prepare uploads its recovery artifacts before pushing the version bump. If that
upload fails, retry prepare from the original checkout; main is unchanged.
Retry failed build/publish jobs in the **same build**, retaining its artifacts.
Do not retry prepare once its bump reached main or start another release with
the same version. Publish resumes matching S3 objects and GitHub draft assets.
There are no automatic retries. Main may advance while builds run; the release
commit must remain its ancestor.

## Activation

The pipeline stays archived until the companion Diode infrastructure is ready.
Follow `infrastructure/terraform/environments/buildkite/pcb-release.md` for
shared CI/deploy workers, AWS roles and GitHub App setup. Run the full
six-target dry run and writer-access checks before enabling publication.
The nightly schedule starts **disabled**. Apply its S3 permissions and worker
hook, then run a six-target nightly dry run before coordinating schedule
enablement with the Actions removal. Retire the old GitHub AWS role afterward.
