//! KiCad text-variable substitution, following `ExpandTextVars` in KiCad's
//! `common/common.cpp`.

const PASSES: usize = 6;
const ESC_DOLLAR: &str = "<<<ESC_DOLLAR:";
const ESC_AT: &str = "<<<ESC_AT:";
/// Escapes and the markers they become; markers from earlier passes stay.
const ESCAPES: [(&str, &str); 4] = [
    (ESC_DOLLAR, ESC_DOLLAR),
    (ESC_AT, ESC_AT),
    ("\\${", ESC_DOLLAR),
    ("\\@{", ESC_AT),
];

/// Replace each `${NAME}` in `value` with `resolve(NAME)`, as KiCad expands
/// text variables. A name may itself hold variables (`${U${ROW}:VALUE}`),
/// and a replacement is expanded again, up to 6 passes, so cycles end.
/// An unresolved `${NAME}` stays as written, `@{...}` expressions are left
/// for the caller, and the escapes `\${` and `\@{` become a literal `${`
/// and `@{`.
pub fn substitute_text_variables<'a>(
    value: &str,
    mut resolve: impl FnMut(&str) -> Option<&'a str>,
) -> String {
    let mut text = value.to_string();
    for _ in 0..PASSES {
        // As KiCad does, `@{` alone still runs a pass, which turns `\@{` into `@{`.
        if !text.contains("${") && !text.contains("@{") {
            break;
        }
        let next = expand(&text, &mut resolve, 0);
        if next == text {
            break;
        }
        text = next;
    }
    text.replace(ESC_DOLLAR, "${").replace(ESC_AT, "@{")
}

/// One expansion pass. Escapes become markers that later passes copy, with
/// their bodies, untouched.
fn expand<'a>(
    text: &str,
    resolve: &mut impl FnMut(&str) -> Option<&'a str>,
    depth: usize,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(['$', '\\', '<']) {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if let Some((prefix, marker)) = ESCAPES.into_iter().find(|(p, _)| rest.starts_with(p)) {
            // The body and its closing brace are copied as written.
            let (_, after) = braced(&rest[prefix.len()..]);
            out.push_str(marker);
            out.push_str(&rest[prefix.len()..rest.len() - after.len()]);
            rest = after;
        } else if let Some(tail) = rest.strip_prefix("${") {
            let (token, after) = braced(tail);
            rest = after;
            if token.is_empty() {
                continue;
            }
            let token = if token.contains("${") && depth < PASSES {
                expand(token, resolve, depth + 1)
            } else {
                token.to_string()
            };
            match resolve(&token) {
                Some(value) => out.push_str(value),
                None => out.push_str(&format!("${{{token}}}")),
            }
        } else {
            out.push_str(&rest[..1]);
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Split `text`, which follows an opening brace, at its matching closing
/// brace: the body, and what follows the brace (empty when unclosed).
fn braced(text: &str) -> (&str, &str) {
    let mut depth = 1;
    for (i, c) in text.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return (&text[..i], &text[i + 1..]);
                }
            }
            _ => {}
        }
    }
    (text, "")
}

#[cfg(test)]
mod tests {
    use super::substitute_text_variables;

    fn resolve(key: &str) -> Option<&'static str> {
        match key {
            "A" => Some("alpha"),
            "B" => Some("${A}"),
            "ROW" => Some("1"),
            "U1:VALUE" => Some("STM32"),
            _ => None,
        }
    }

    #[test]
    fn substitutes_nested_and_recursive_variables() {
        assert_eq!(
            substitute_text_variables("${U${ROW}:VALUE}", resolve),
            "STM32"
        );
        assert_eq!(substitute_text_variables("${B}", resolve), "alpha");
        assert_eq!(
            substitute_text_variables("${U${ROW}:MISSING}", resolve),
            "${U1:MISSING}"
        );
        assert_eq!(
            substitute_text_variables("${U1:NET_NAME(@{${ROW}-2})}", resolve),
            "${U1:NET_NAME(@{1-2})}"
        );
    }

    #[test]
    fn preserves_escaped_variables_and_bounds_cycles() {
        fn self_ref(key: &str) -> Option<&'static str> {
            (key == "LOOP").then_some("${LOOP}")
        }

        assert_eq!(
            substitute_text_variables(r"\${A} ${A}", resolve),
            "${A} alpha"
        );
        assert_eq!(substitute_text_variables(r"\@{ROW}", resolve), "@{ROW}");
        assert_eq!(substitute_text_variables("${LOOP}", self_ref), "${LOOP}");
    }
}
