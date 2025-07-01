//! Telemetry support for PCB tools with Sentry and PostHog integration
//!
//! This crate provides telemetry functionality that is only active in release builds.
//! Users can opt-out of telemetry by setting the PCB_TELEMETRY environment variable to "off".

use anyhow::Result;
use once_cell::sync::Lazy;
use std::sync::Mutex;

#[cfg(all(feature = "telemetry", not(debug_assertions)))]
use gethostname::gethostname;

#[cfg(all(feature = "telemetry", not(debug_assertions)))]
use posthog_rs::Event;

#[cfg(all(feature = "telemetry", not(debug_assertions)))]
use sentry::ClientInitGuard;

/// Sentry DSN for error reporting
#[cfg(all(feature = "telemetry", not(debug_assertions)))]
const SENTRY_DSN: &str = "https://5d5c576f68c44baca59fc5352d3e40b5@o4508175627059200.ingest.us.sentry.io/4508175629352960";

/// PostHog API key for analytics
#[cfg(all(feature = "telemetry", not(debug_assertions)))]
const POSTHOG_KEY: &str = "phc_h5EuYy42Vt2Qm5aOxx7ajD8inYEtj88v0KY8rwmcXhC";

/// Checks if telemetry is enabled based on environment variable
pub fn is_telemetry_enabled() -> bool {
    std::env::var("PCB_TELEMETRY")
        .map(|val| val.to_lowercase() != "off")
        .unwrap_or(true)
}

/// Gets or creates a persistent anonymous user ID
fn get_anonymous_id() -> String {
    // For now, generate a new ID each time. In the future, this could be persisted
    // in a config file in the user's home directory
    uuid::Uuid::new_v4().to_string()
}

/// Global telemetry state
static TELEMETRY_STATE: Lazy<Mutex<TelemetryState>> = Lazy::new(|| {
    Mutex::new(TelemetryState {
        #[cfg(all(feature = "telemetry", not(debug_assertions)))]
        _guard: None,
        initialized: false,
    })
});

struct TelemetryState {
    #[cfg(all(feature = "telemetry", not(debug_assertions)))]
    _guard: Option<ClientInitGuard>,
    initialized: bool,
}

/// Initializes telemetry
///
/// This function should be called once at the start of the application.
/// In debug builds, this is a no-op. In release builds, it initializes
/// Sentry and sends an initial PostHog event.
pub fn init_telemetry() -> Result<()> {
    if !is_telemetry_enabled() {
        log::debug!("Telemetry is disabled via PCB_TELEMETRY environment variable");
        return Ok(());
    }

    let mut state = TELEMETRY_STATE.lock().unwrap();
    if state.initialized {
        log::debug!("Telemetry already initialized");
        return Ok(());
    }

    #[cfg(all(feature = "telemetry", not(debug_assertions)))]
    {
        // Initialize Sentry
        let guard = sentry::init((
            SENTRY_DSN,
            sentry::ClientOptions {
                release: sentry::release_name!(),
                ..Default::default()
            },
        ));
        
        // Configure Sentry scope
        configure_sentry()?;
        
        // Send initial PostHog event
        track_invocation()?;
        
        state._guard = Some(guard);
    }
    
    state.initialized = true;
    Ok(())
}

/// Configures Sentry scope with command information
#[cfg(all(feature = "telemetry", not(debug_assertions)))]
fn configure_sentry() -> Result<()> {
    sentry::configure_scope(|scope| {
        scope.set_tag(
            "command",
            format!("{:?}", std::env::args().collect::<Vec<String>>().join(" ")),
        );
        scope.set_tag("path", std::env::current_dir().unwrap().to_string_lossy());
        scope.set_user(Some(sentry::User {
            id: Some(get_anonymous_id()),
            ..Default::default()
        }));
    });
    Ok(())
}

/// Tracks a command invocation in PostHog
#[cfg(all(feature = "telemetry", not(debug_assertions)))]
fn track_invocation() -> Result<()> {
    let anonymous_id = get_anonymous_id();
    let client = posthog_rs::client(POSTHOG_KEY);
    let mut event = Event::new("invocation", &anonymous_id);
    
    event.insert_prop(
        "command",
        std::env::args().collect::<Vec<String>>().join(" "),
    )?;
    event.insert_prop("path", std::env::current_dir().unwrap().to_string_lossy())?;
    event.insert_prop("hostname", format!("{:?}", gethostname()))?;
    event.insert_prop("version", env!("CARGO_PKG_VERSION"))?;
    
    client.capture(event)?;
    Ok(())
}

/// Sets up the logger with Sentry integration
///
/// This creates a logger that sends errors to Sentry and other log levels
/// as breadcrumbs. The actual log level is controlled by the RUST_LOG
/// environment variable, defaulting to "warn".
pub fn setup_logger() -> Result<()> {
    // Build env logger with default level set to warn
    let mut log_builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn")
    );
    
    #[cfg(all(feature = "telemetry", not(debug_assertions)))]
    if is_telemetry_enabled() {
        let sentry_logger = sentry::integrations::log::SentryLogger::with_dest(log_builder.build())
            .filter(|md| match md.level() {
                log::Level::Error => sentry::integrations::log::LogFilter::Event,
                _ => sentry::integrations::log::LogFilter::Breadcrumb,
            });
        
        // Set the global logger
        log::set_boxed_logger(Box::new(sentry_logger))?;
        log::set_max_level(log::LevelFilter::Debug);
    } else {
        log_builder.init();
    }
    
    #[cfg(debug_assertions)]
    log_builder.init();
    
    Ok(())
}

/// Captures an error and sends it to Sentry (in release builds)
///
/// This is a convenience function for capturing errors. In debug builds,
/// this is a no-op.
pub fn capture_error(error: &anyhow::Error) {
    #[cfg(all(feature = "telemetry", not(debug_assertions)))]
    if is_telemetry_enabled() {
        sentry::integrations::anyhow::capture_anyhow(error);
    }
}

/// Tracks a custom event in PostHog
///
/// This allows tracking custom events beyond the initial invocation.
/// In debug builds, this is a no-op.
#[cfg(all(feature = "telemetry", not(debug_assertions)))]
pub fn track_event(event_name: &str, properties: Option<serde_json::Value>) -> Result<()> {
    if !is_telemetry_enabled() {
        return Ok(());
    }
    
    let anonymous_id = get_anonymous_id();
    let client = posthog_rs::client(POSTHOG_KEY);
    let mut event = Event::new(event_name, &anonymous_id);
    
    if let Some(props) = properties {
        if let Some(obj) = props.as_object() {
            for (key, value) in obj {
                event.insert_prop(key, value)?;
            }
        }
    }
    
    client.capture(event)?;
    Ok(())
}

#[cfg(debug_assertions)]
pub fn track_event(_event_name: &str, _properties: Option<serde_json::Value>) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telemetry_enabled_by_default() {
        // Save current env var
        let original = std::env::var("PCB_TELEMETRY").ok();
        
        // Remove env var
        std::env::remove_var("PCB_TELEMETRY");
        assert!(is_telemetry_enabled());
        
        // Restore original
        if let Some(val) = original {
            std::env::set_var("PCB_TELEMETRY", val);
        }
    }

    #[test]
    fn test_telemetry_can_be_disabled() {
        // Save current env var
        let original = std::env::var("PCB_TELEMETRY").ok();
        
        // Set to off
        std::env::set_var("PCB_TELEMETRY", "off");
        assert!(!is_telemetry_enabled());
        
        // Set to OFF (uppercase)
        std::env::set_var("PCB_TELEMETRY", "OFF");
        assert!(!is_telemetry_enabled());
        
        // Set to something else
        std::env::set_var("PCB_TELEMETRY", "on");
        assert!(is_telemetry_enabled());
        
        // Restore original
        if let Some(val) = original {
            std::env::set_var("PCB_TELEMETRY", val);
        } else {
            std::env::remove_var("PCB_TELEMETRY");
        }
    }
}