# KiCad Test Fixtures

Third-party KiCad projects vendored **unmodified** for testing `pcb import`.

Each subdirectory is a complete KiCad project with its original license preserved.
These files are not modified, not used commercially, and exist solely as test
inputs for the Zener toolchain.

## Adding a new fixture

1. Copy the project directory into this folder.
2. Ensure the original `LICENSE` (or equivalent) file is included.
3. Run `./check-licenses.sh --generate` to update `LICENSES.spdx`.
