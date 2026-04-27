mod app;
mod availability;
pub(crate) mod display;
mod image;
pub mod search;
mod ui;

pub use app::{
    SearchMode, TuiResult, run, run_web_components_only, run_with_mode,
    run_with_mode_and_registry_index,
};
