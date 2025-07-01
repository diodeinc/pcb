# pcb-telem

Telemetry support for PCB tools with Sentry and PostHog integration.

## Features

- **Error Reporting**: Automatic error reporting to Sentry in release builds
- **Analytics**: Usage analytics via PostHog
- **Privacy First**: Opt-out telemetry with environment variable control
- **Debug/Release Separation**: Telemetry only active in release builds

## Usage

```rust
use pcb_telem::{init_telemetry, setup_logger, capture_error};

fn main() -> Result<()> {
    // Setup logger with Sentry integration
    setup_logger()?;
    
    // Initialize telemetry
    init_telemetry()?;
    
    // Your application code here
    if let Err(e) = run_app() {
        // Capture errors to Sentry
        capture_error(&e);
        return Err(e);
    }
    
    Ok(())
}
```

## Opting Out

Users can disable telemetry by setting the `PCB_TELEMETRY` environment variable:

```bash
export PCB_TELEMETRY=off
```

## Privacy

When telemetry is enabled, we collect:
- Command invocations
- Error messages and stack traces
- Basic system info (OS type, tool version)
- Anonymous usage ID

We never collect:
- Source code or design files
- Personal file paths
- Network information
- Credentials

## Implementation Details

- Telemetry is compiled out in debug builds using `cfg(not(debug_assertions))`
- All telemetry functions are no-ops when disabled
- Sentry is used for error reporting
- PostHog is used for usage analytics