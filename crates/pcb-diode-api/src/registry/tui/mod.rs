//! Interactive TUI for registry search
//!
//! Architecture:
//! - Main thread: handles UI rendering and input events
//! - Worker thread: handles search queries against all indices
//! - Communication via mpsc channels (query -> worker, results <- worker)
//!
//! Layout:
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │ Search: [____________________]                              │
//! ├───────────────────┬───────────────────┬─────────────────────┤
//! │ Trigram Results   │ Word Results      │ Merged Results      │
//! │                   │                   │                     │
//! │ - STM32G431RBT6   │ - STM32G431RBT6   │ - STM32G431RBT6     │
//! │ - STM32G431CBU6   │ - STM32G431CBU6   │ - STM32G431CBU6     │
//! │                   │                   │                     │
//! ├───────────────────┴───────────────────┴─────────────────────┤
//! │ 357 parts │ Query: 1.2ms │ Press Esc to quit                │
//! └─────────────────────────────────────────────────────────────┘
//! ```

mod app;
mod image;
mod search;
mod ui;

pub use app::run;
