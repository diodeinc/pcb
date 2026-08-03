use lsp_types::{Position, Range};
use starlark::codemap::ResolvedSpan;

pub(crate) trait IntoLspRange {
    fn to_lsp_range(self) -> Range;
}

impl IntoLspRange for ResolvedSpan {
    fn to_lsp_range(self) -> Range {
        Range::new(
            Position::new(self.begin.line as u32, self.begin.column as u32),
            Position::new(self.end.line as u32, self.end.column as u32),
        )
    }
}

impl IntoLspRange for Range {
    fn to_lsp_range(self) -> Range {
        self
    }
}
