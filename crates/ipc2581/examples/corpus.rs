//! Parse every given IPC-2581 file (`.xml` or `.xml.zst`) and print one line
//! per file: a hash of the typed model, or the parse error.
//!
//! The hash covers the `Debug` form of the model with every `Symbol`
//! replaced by its string, so it is stable across runs and across changes to
//! interning order. `--dump <dir>` also writes that text for diffing.
//!
//! ```text
//! cargo run --release -p ipc2581 --example corpus -- [--dump <dir>] <files…>
//! ```

use std::fmt::{self, Debug, Write};
use std::hash::{DefaultHasher, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};

use ipc2581::{Interner, Ipc2581, Symbol};

/// `fmt::Write` sink that resolves `Symbol(n)` on the fly. `derive(Debug)`
/// emits the name, `(`, the index and `)` as separate writes.
struct ModelText<'a> {
    interner: &'a Interner,
    /// A `Symbol` is a dense interning index, so the `n`th distinct string
    /// interned anywhere yields the handle for index `n`.
    scratch: Interner,
    symbols: Vec<Symbol>,
    pending: u8,
    hasher: DefaultHasher,
    dump: Option<String>,
}

impl ModelText<'_> {
    fn emit(&mut self, text: &str) {
        self.hasher.write(text.as_bytes());
        if let Some(dump) = &mut self.dump {
            dump.push_str(text);
        }
    }
}

impl Write for ModelText<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        match (self.pending, text) {
            (0, "Symbol") => self.pending = 1,
            (1, "(") => self.pending = 2,
            (2, index) => {
                let index = index.parse::<usize>().map_err(|_| fmt::Error)?;
                for n in self.symbols.len()..=index {
                    self.symbols.push(self.scratch.intern(&n.to_string()));
                }
                let resolved = format!("{:?}", self.interner.resolve(self.symbols[index]));
                self.emit(&resolved);
                self.pending = 3;
            }
            (3, ")") => self.pending = 0,
            (0, text) => self.emit(text),
            _ => return Err(fmt::Error),
        }
        Ok(())
    }
}

fn read(path: &Path) -> std::io::Result<String> {
    let mut text = String::new();
    let mut file = std::fs::File::open(path)?;
    if path.extension().is_some_and(|ext| ext == "zst") {
        zstd::Decoder::new(file)?.read_to_string(&mut text)?;
    } else {
        file.read_to_string(&mut text)?;
    }
    Ok(text)
}

fn model_text(ipc: &Ipc2581, dump: bool) -> (u64, Option<String>) {
    let interner = ipc.interner();
    let mut out = ModelText {
        interner,
        scratch: Interner::new(),
        symbols: Vec::new(),
        pending: 0,
        hasher: DefaultHasher::new(),
        dump: dump.then(String::new),
    };
    let sections: [&dyn Debug; 5] = [
        &ipc.content(),
        &ipc.logistic_header(),
        &ipc.history_record(),
        &ipc.boms(),
        &ipc.avl(),
    ];
    for section in sections {
        writeln!(out, "{section:?}").expect("model formats");
    }
    if let Some(ecad) = ipc.ecad() {
        // `specs` is keyed by a randomly seeded hash; print it in name order.
        let mut header = ecad.cad_header.clone();
        let mut specs = header
            .specs
            .drain()
            .map(|(_, spec)| spec)
            .collect::<Vec<_>>();
        specs.sort_by_key(|spec| interner.resolve(spec.name));
        writeln!(out, "{header:?}\n{specs:?}\n{:?}", ecad.cad_data).expect("model formats");
    }
    (out.hasher.finish(), out.dump)
}

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let dump_dir = (args.peek().map(String::as_str) == Some("--dump")).then(|| {
        args.next();
        PathBuf::from(args.next().expect("--dump takes a directory"))
    });
    let mut failed = 0;
    for path in args.map(PathBuf::from) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        match read(&path)
            .map_err(ipc2581::Ipc2581Error::from)
            .and_then(|xml| Ipc2581::parse(&xml))
        {
            Ok(ipc) => {
                let (hash, dump) = model_text(&ipc, dump_dir.is_some());
                println!("{name}\tok\t{hash:016x}");
                if let (Some(dir), Some(dump)) = (&dump_dir, dump) {
                    std::fs::write(dir.join(format!("{name}.txt")), dump).expect("dump writes");
                }
            }
            Err(error) => {
                failed += 1;
                println!("{name}\tERR\t{error}");
            }
        }
    }
    std::process::exit(i32::from(failed > 0));
}
