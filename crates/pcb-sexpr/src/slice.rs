//! In-memory scanner: copy complete UTF-8 runs, not individual bytes.
use super::*;

pub(super) fn parse(input: &str, root_limit: Option<usize>) -> Result<Vec<Sexpr>, ParseError> {
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut stack = Vec::<StreamListFrame>::new();
    let mut roots = Vec::new();
    while i < bytes.len() {
        let start = i;
        let node = match bytes[i] {
            b'(' => {
                stack.push(StreamListFrame {
                    start,
                    items: Vec::new(),
                });
                i += 1;
                continue;
            }
            b')' => {
                let frame = stack.pop().ok_or(ParseError::UnexpectedChar(')', '('))?;
                i += 1;
                Sexpr::with_span(SexprKind::List(frame.items), Span::new(frame.start, i))
            }
            b';' => {
                i += bytes[i..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .unwrap_or(bytes.len() - i);
                continue;
            }
            b if b.is_ascii_whitespace() => {
                i += 1;
                continue;
            }
            b'"' => {
                i += 1;
                let mut run = i;
                // Allocate only when escapes require decoding; plain strings get one copy.
                let mut decoded: Option<String> = None;
                loop {
                    i += bytes[i..]
                        .iter()
                        .position(|&b| b == b'"' || b == b'\\')
                        .unwrap_or(bytes.len() - i);
                    if i == bytes.len() {
                        return Err(ParseError::UnterminatedString);
                    }
                    if bytes[i] == b'"' {
                        let value = if let Some(mut value) = decoded {
                            value.push_str(&input[run..i]);
                            value
                        } else {
                            input[run..i].to_owned()
                        };
                        i += 1;
                        break Sexpr::with_span(SexprKind::String(value), Span::new(start, i));
                    }
                    let value = decoded.get_or_insert_with(String::new);
                    value.push_str(&input[run..i]);
                    i += 1;
                    if i == bytes.len() {
                        return Err(ParseError::UnterminatedString);
                    }
                    // Unknown escapes discard the backslash, including before Unicode.
                    let ch = input[i..].chars().next().unwrap();
                    value.push(match ch {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        other => other,
                    });
                    i += ch.len_utf8();
                    run = i;
                }
            }
            _ => {
                // Quotes and semicolons INSIDE atoms are not delimiters in the stream parser.
                i += bytes[i..]
                    .iter()
                    .position(|&b| b.is_ascii_whitespace() || b == b'(' || b == b')')
                    .unwrap_or(bytes.len() - i);
                parse_owned_atom(input[start..i].to_owned(), Span::new(start, i))
            }
        };
        if !stream_finish_node(&mut stack, &mut roots, root_limit, node, &mut |_| true) {
            return Ok(roots);
        }
    }
    if !stack.is_empty() {
        return Err(ParseError::UnclosedList);
    }
    Ok(roots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        hint::black_box,
        io::{BufReader, Cursor},
        path::Path,
        time::Instant,
    };

    fn same_nodes(a: &Sexpr, b: &Sexpr) {
        assert_eq!(a.span, b.span);
        assert_eq!(a.raw_atom, b.raw_atom);
        match (&a.kind, &b.kind) {
            (SexprKind::F64(a), SexprKind::F64(b)) => assert_eq!(a.to_bits(), b.to_bits()),
            (SexprKind::List(a), SexprKind::List(b)) => {
                assert_eq!(a.len(), b.len());
                for (a, b) in a.iter().zip(b) {
                    same_nodes(a, b);
                }
            }
            (a, b) => assert_eq!(a, b),
        }
    }

    fn differential(input: &str) {
        for limit in [None, Some(1)] {
            let fast = parse(input, limit);
            for capacity in [1, 7, 8192] {
                let slow = finish_in_memory_parse(parse_stream(
                    BufReader::with_capacity(capacity, Cursor::new(input)),
                    limit,
                    |_| true,
                ));
                match (&fast, &slow) {
                    (Ok(a), Ok(b)) => {
                        assert_eq!(a.len(), b.len(), "{input:?}");
                        for (a, b) in a.iter().zip(b) {
                            same_nodes(a, b);
                        }
                    }
                    (Err(a), Err(b)) => assert_eq!(a, b, "{input:?}"),
                    _ => panic!("mismatch for {input:?}: {fast:?} / {slow:?}"),
                }
            }
        }
    }

    #[test]
    fn generated_differential() {
        let pieces = [
            "(",
            ")",
            " ",
            "\n",
            "\r",
            "\t",
            "\x0b",
            "\x0c",
            ";comment\n",
            ";",
            "\"",
            "\\",
            "\\n",
            "\\r",
            "\\t",
            "\\\"",
            "\\\\",
            "\\é",
            "é",
            "日本語",
            "🔥",
            "\0",
            "\u{a0}",
            "foo;bar",
            "a\"b",
            "-0.0",
            "+001",
            "12.000000",
            "NaN",
            "inf",
            "-inf",
            "1e999",
            "1e-999",
            "9223372036854775808",
        ];
        for piece in pieces {
            differential(piece);
            differential(&format!("({piece})"));
        }
        let valid = [
            "()",
            "(nested (a 1) (b -0.0))",
            "foo;bar",
            "a\"b",
            "日本語",
            r#""plain🔥""#,
            r#""\n\r\t\\\"\q\é\🔥""#,
            "+0001",
            "-9223372036854775808",
            "9223372036854775807",
            "-9223372036854775809",
            "1e+5",
            "1e-999",
            "-0.0",
            "NaN",
            "nan",
            "+inf",
            "-Infinity",
            "1e999",
            "0x12",
            "1.2.3",
        ];
        for a in valid {
            for b in valid {
                let input = format!("(root {a} ; comment )\"\n {b}) {a} {b}");
                assert!(parse(&input, None).is_ok());
                differential(&input);
                // Truncated valid trees exercise EOF and error precedence at UTF-8 boundaries.
                for (end, _) in input.char_indices() {
                    differential(&input[..end]);
                }
            }
        }
        let mut rng = 0x12345678u64;
        for _ in 0..20000 {
            let mut input = String::new();
            for _ in 0..(rng as usize % 40) {
                rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
                input.push_str(pieces[(rng >> 32) as usize % pieces.len()]);
            }
            differential(&input);
            differential(&format!("(root {input}) trailing )"));
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        differential(&format!(
            "({} \"{}\")",
            "é".repeat(100000),
            "abc🔥".repeat(100000)
        ));
        differential(&format!("{}x{}", "(".repeat(512), ")".repeat(512)));
    }

    fn files(path: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, out);
            } else if matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("kicad_sch" | "kicad_pcb" | "kicad_sym" | "kicad_mod" | "net")
            ) {
                out.push(path);
            }
        }
    }

    // Native feasibility benchmark: same AST construction/drop, input I/O excluded.
    // PCB_SEXPR_CORPUS must point at extracted schematic sources (not JSON fixtures).
    // Run release with an absolute corpus path and --ignored --nocapture.
    #[test]
    #[ignore]
    fn corpus_benchmark() {
        let mut paths = Vec::new();
        files(
            Path::new(&std::env::var("PCB_SEXPR_CORPUS").unwrap()),
            &mut paths,
        );
        paths.sort();
        assert!(!paths.is_empty());
        let mut total_bytes = 0;
        let mut totals = [std::time::Duration::ZERO; 2];
        for path in &paths {
            let input = std::fs::read_to_string(path).unwrap();
            let slow = finish_in_memory_parse(parse_stream(Cursor::new(&input), None, |_| true));
            let fast = parse(&input, None);
            match (&fast, &slow) {
                (Ok(a), Ok(b)) => {
                    assert_eq!(a.len(), b.len());
                    for (a, b) in a.iter().zip(b) {
                        same_nodes(a, b);
                    }
                }
                (Err(a), Err(b)) => assert_eq!(a, b),
                _ => panic!("corpus mismatch {}", path.display()),
            }
            drop((slow, fast));
            let mut samples = [Vec::new(), Vec::new()];
            for round in 0..7 {
                for mode in [round % 2, 1 - round % 2] {
                    let start = Instant::now();
                    if mode == 0 {
                        black_box(parse_stream(Cursor::new(black_box(&input)), None, |_| true))
                            .unwrap();
                    } else {
                        black_box(parse(black_box(&input), None)).unwrap();
                    }
                    samples[mode].push(start.elapsed());
                }
            }
            for s in &mut samples {
                s.sort();
            }
            total_bytes += input.len();
            totals[0] += samples[0][3];
            totals[1] += samples[1][3];
            println!(
                "{} bytes={} stream_ms={:.3} slice_ms={:.3} speedup={:.2}",
                path.display(),
                input.len(),
                samples[0][3].as_secs_f64() * 1000.,
                samples[1][3].as_secs_f64() * 1000.,
                samples[0][3].as_secs_f64() / samples[1][3].as_secs_f64()
            );
        }
        println!(
            "TOTAL files={} bytes={} stream_ms={:.3} slice_ms={:.3} speedup={:.2}",
            paths.len(),
            total_bytes,
            totals[0].as_secs_f64() * 1000.,
            totals[1].as_secs_f64() * 1000.,
            totals[0].as_secs_f64() / totals[1].as_secs_f64()
        );
    }
}
