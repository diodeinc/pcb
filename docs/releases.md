# PCB releases

Start a manual `pcb-release` Buildkite build from current `main`. Select
`compiler` or `shim`, enter an increasing SemVer, and leave **Dry run** on to
build and verify without writer credentials or GitHub/S3 writes.

The pipeline bumps the version on main, builds six native targets from that
commit, verifies and uploads immutable S3 artifacts, writes the completion
marker/latest pointer, then pushes the tag. Compiler releases also publish a
verified GitHub Release; shims stay S3-only.

## Nightly

The same `pcb-release` pipeline builds nightly from `main` with build environment
`PCB_RELEASE_CHANNEL=nightly`. It bypasses the version form and reuses the stable
six-target builders, stdlib packaging, checksums and immutable S3 uploads. It does
not bump versions, push commits/tags, or acquire GitHub App credentials.

The daily schedule is **07:17 UTC**. Prepare checks `pcb/nightly/latest.json` and
skips all builders when its SHA is unchanged. After verifying every target,
publish updates that pointer with the existing download URL/JSON contract. Failed
uploads cannot expose an incomplete nightly; older retries cannot downgrade it.
Manual dry runs use both `PCB_RELEASE_CHANNEL=nightly` and
`PCB_RELEASE_DRY_RUN=true`; they build even an unchanged SHA without AWS credentials
or publication. Dry runs must also use `main`.

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
The companion infrastructure adds the nightly schedule **disabled** for a staged
cutover. Apply its S3 permissions and worker admission changes, run a nightly
six-target dry run, then enable the schedule as the Actions nightly workflow is
removed. Do not land the Actions removal before Buildkite is ready. The former
GitHub nightly AWS role can be retired separately after successful publication.
