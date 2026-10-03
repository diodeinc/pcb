//! The fonts pcb bundles, each under the SIL Open Font License beside it in
//! `fonts/`: Liberation Sans, metric-compatible with Arial, which boards
//! are lettered in; and Roboto Mono, which drawings are.

pub static LIBERATION_SANS_REGULAR: &[u8] = include_bytes!("../fonts/LiberationSans-Regular.ttf");
pub static LIBERATION_SANS_ITALIC: &[u8] = include_bytes!("../fonts/LiberationSans-Italic.ttf");
pub static LIBERATION_SANS_BOLD: &[u8] = include_bytes!("../fonts/LiberationSans-Bold.ttf");
pub static LIBERATION_SANS_BOLD_ITALIC: &[u8] =
    include_bytes!("../fonts/LiberationSans-BoldItalic.ttf");

pub static ROBOTO_MONO_REGULAR: &[u8] = include_bytes!("../fonts/RobotoMono-Regular.ttf");
pub static ROBOTO_MONO_BOLD: &[u8] = include_bytes!("../fonts/RobotoMono-Bold.ttf");
