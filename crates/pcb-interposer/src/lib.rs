//! Fixture-interposer POC: extract → bundle → Hall → assign → pattern → route.

pub mod assign;
pub mod bundle;
pub mod emit;
pub mod extract;
pub mod geom;
pub mod hall;
pub mod instantiate;
pub mod panel;
pub mod pattern;
pub mod route;
pub mod router;
pub mod score;
pub mod types;
pub mod viz;

pub use assign::assign;
pub use bundle::bundle;
pub use extract::{extract_ipc_xml, extract_kicad_src, is_bottom_copper, parse_zen_ict_map};
pub use hall::hall;
pub use instantiate::{Sheet, instantiate, pack};
pub use pattern::{PatternKind, generate_pattern, generate_pattern_at};
pub use route::{RouteResult, Trace, TwoPinNet, nets_from_assign};
pub use router::route_r5;
pub use score::{KindCov, Score, quality_score, score_g0, score_g1};
pub use types::*;
