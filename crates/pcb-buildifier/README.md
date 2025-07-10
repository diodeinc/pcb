# pcb-buildifier

This crate provides a bundled [buildifier](https://github.com/bazelbuild/buildtools/tree/master/buildifier) binary for use by the PCB tooling ecosystem.

## About Buildifier

Buildifier is a tool for formatting Bazel BUILD and .bzl files with a standard convention. It's part of the [buildtools](https://github.com/bazelbuild/buildtools) project by the Bazel Authors.

## How It Works

This crate:

1. Downloads the appropriate buildifier binary for the target platform during the build process
2. Embeds the binary directly into the Rust binary at compile time
3. On first use, extracts the binary to a cache directory (`~/.cache/pcb/buildifier/v<version>/` on Linux/macOS, `~/Library/Caches/pcb/buildifier/v<version>/` on macOS, `%LOCALAPPDATA%\pcb\buildifier\v<version>\` on Windows)
4. Subsequent runs use the cached binary for optimal performance

This approach ensures that:

- No internet connection is required at runtime
- The buildifier version is consistent across all platforms
- No PATH configuration is needed
- The tool works out of the box
- Performance is optimal after the first run (typically 10-20x faster)

## Supported Platforms

- macOS (x86_64, ARM64)
- Linux (x86_64, ARM64)
- Windows (x86_64)

## Cache Management

The buildifier binary is cached to improve performance. If you need to clear the cache (e.g., if the binary becomes corrupted), you can delete the cache directory:

- **macOS**: `rm -rf ~/Library/Caches/pcb/buildifier/`
- **Linux**: `rm -rf ~/.cache/pcb/buildifier/`
- **Windows**: `rmdir /s %LOCALAPPDATA%\pcb\buildifier\`

The binary will be re-extracted on the next run.

## License

This crate is licensed under the MIT license (see the root LICENSE file).

The bundled buildifier binary is part of the buildtools project and is licensed under the Apache License, Version 2.0. See the LICENSE file in this directory for the full Apache 2.0 license text.

## Usage

This crate is primarily intended to be used as a dependency by other PCB tools, particularly `pcb fmt`.

```rust
use pcb_buildifier::Buildifier;

let buildifier = Buildifier::new()?;
buildifier.format_file(&path)?;
```
