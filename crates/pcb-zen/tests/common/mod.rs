use std::path::{Path, PathBuf};

use pcb_zen_core::WithDiagnostics;

/// Utility to build an isolated Starlark project for integration tests.
///
/// The helper wraps a [`tempfile::TempDir`] so each invocation gets its own
/// sandbox on disk.  Test code can incrementally add files via [`Self::add_file`]
/// and then evaluate the project with [`Self::eval_netlist`].
///
/// ```no_run
/// use common::TestProject;
/// use insta::assert_snapshot;
///
/// let env = TestProject::new();
/// // Write a sub-module.
/// env.add_file(
///     "sub.zen",
///     r#"Component(footprint = "TEST:0402", pins = {"1": PinSpec("p", "1")})"#,
/// );
/// // Write a top-level module that loads the sub-module.
/// env.add_file(
///     "top.zen",
///     r#"Sub = Module("sub.zen")
/// Sub()"#,
/// );
///
/// let netlist = env.eval_netlist("top.zen");
/// assert_snapshot!(netlist);
/// ```
///
/// Note: the helper panics on IO or evaluation errors to keep the tests concise.
pub struct TestProject {
    _temp_dir: tempfile::TempDir,
    root: PathBuf,
}

impl TestProject {
    /// Create a fresh temporary project directory.
    pub fn new() -> Self {
        let _temp_dir = tempfile::tempdir().expect("failed to create temp dir for test project");
        let root = _temp_dir
            .path()
            .canonicalize()
            .expect("failed to canonicalize temp dir");

        Self { _temp_dir, root }
    }

    /// Absolute path to the root of the temporary project directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Write `contents` to a file at `rel_path` within the project root.
    ///
    /// Any intermediate directories are created automatically.  The full path to the
    /// newly-written file is returned.
    pub fn add_file(&self, rel_path: impl AsRef<Path>, contents: &str) -> PathBuf {
        let path = self.root().join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("failed to create parent dirs: {e}"));
        }
        std::fs::write(&path, contents)
            .unwrap_or_else(|e| panic!("failed to write test file {path:?}: {e}"));
        path
    }

    /// Evaluate the Starlark project starting from `top_rel_path` (relative to the
    /// project root) and return the generated KiCad netlist as a string suitable
    /// for snapshot testing.
    pub fn eval_netlist(&self, top_rel_path: impl AsRef<Path>) -> WithDiagnostics<String> {
        let top_path = self.root().join(top_rel_path);
        self.eval_netlist_from_absolute(&top_path)
    }

    /// Same as [`Self::eval_netlist`] but accepts an absolute path.  This is useful
    /// when a test already has a full path (e.g. returned from [`Self::add_file`]).
    pub fn eval_netlist_from_absolute(&self, top_path: &Path) -> WithDiagnostics<String> {
        use pcb_sch::kicad_netlist::to_kicad_netlist;
        pcb_zen::run(top_path).map(|s| to_kicad_netlist(&s))
    }

    /// Parse a single text blob that contains multiple files and write them into
    /// this [`TestProject`].
    ///
    /// The blob must use *file delimiters* consisting of a Starlark‐style comment
    /// that appears exactly at the **start of a line**:
    ///
    /// ```text
    /// # --- path/to/file.ext
    /// ```
    ///
    /// Everything that follows the delimiter – until the next delimiter or the
    /// end of the blob – becomes the contents of `path/to/file.ext`.  Leading and
    /// trailing whitespace within each file is preserved so the helper works for
    /// arbitrary text such as KiCad symbol libraries.
    ///
    /// Example
    /// -------
    /// ```text
    /// # --- C146731.kicad_sym
    ///  (kicad symbol content ...)
    /// # --- sub.zen
    /// COMP = load_component("C146731.kicad_sym", footprint = "SMD:0805")
    /// # --- top.zen
    /// Sub = Module("sub.zen")
    /// Sub()
    /// ```
    #[allow(dead_code)]
    pub fn add_files_from_blob(&self, blob: &str) {
        let mut current_path: Option<String> = None;
        let mut buffer = String::new();

        for line in blob.lines() {
            if let Some(stripped) = line.strip_prefix("# --- ") {
                // Flush any previous file we were collecting.
                if let Some(path) = current_path.take() {
                    self.add_file(&path, &buffer);
                    buffer.clear();
                }
                current_path = Some(stripped.trim().to_owned());
            } else {
                buffer.push_str(line);
                buffer.push('\n');
            }
        }

        // Flush the final file.
        if let Some(path) = current_path {
            self.add_file(&path, &buffer);
        }
    }
}

/// Convenience macro to create a [`TestProject`], load a blob of files with
/// [`add_files_from_blob`], evaluate the given *entry* file and snapshot the
/// resulting KiCad netlist with `insta`.
///
/// Usage
/// -----
/// ```ignore
/// use common::star_snapshot;
/// let env = TestProject::new();
/// env.add_files_from_blob(r"""
/// # --- sub.zen
/// Component(footprint = "TEST:0402", pins = {"1": PinSpec("p", "1")})
/// # --- top.zen
/// Sub = Module("sub.zen")
/// Sub()
/// """);
/// star_snapshot!(env, "top.zen");
/// ```
#[macro_export]
macro_rules! star_snapshot {
    ($env:expr, $entry:expr $(,)?) => {{
        let netlist = $env.eval_netlist($entry);
        let root_path = $env.root().to_string_lossy();
        insta::with_settings!({
            filters => vec![
                (
                    ::regex::escape(&format!("{}{}", root_path, std::path::MAIN_SEPARATOR)).as_ref(),
                    "[TEMP_DIR]",
                ),
            ],
        }, {
            insta::assert_snapshot!(netlist);
        });
    }};
}
