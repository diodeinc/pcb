use pcb_sch::BoardSide;
use std::fs;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct MechanicalPose {
    pub x_mm: f64,
    pub y_mm: f64,
    pub rotation_deg: f64,
    pub side: BoardSide,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IdfPlacementClaim {
    pub refdes: String,
    pub package: String,
    pub part_number: Option<String>,
    pub pose: MechanicalPose,
    /// IDF placement status `MCAD`: mechanical owns this component's position.
    pub mcad_owned: bool,
}

pub(crate) fn load_placement_claims(path: &Path) -> anyhow::Result<Vec<IdfPlacementClaim>> {
    let source = fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read IDF board file {}: {e}", path.display()))?;
    parse_placement_claims(&source)
        .map_err(|e| anyhow::anyhow!("failed to parse IDF board file {}: {e}", path.display()))
}

#[derive(Debug, Error)]
pub enum IdfError {
    #[error("IDF file does not contain a .PLACEMENT section")]
    MissingPlacementSection,
    #[error("line {line}: malformed .PLACEMENT package record; expected 3 fields, got {count}")]
    MalformedPackageRecord { line: usize, count: usize },
    #[error(
        "line {line}: malformed .PLACEMENT coordinate record; expected at least 6 fields, got {count}"
    )]
    MalformedCoordinateRecord { line: usize, count: usize },
    #[error(
        "line {line}: package record was not followed by a coordinate record before .END_PLACEMENT"
    )]
    MissingCoordinateRecord { line: usize },
    #[error("line {line}: invalid {field} value {value:?}")]
    InvalidNumber {
        line: usize,
        field: &'static str,
        value: String,
    },
    #[error("line {line}: unknown board side {value:?}; expected TOP or BOTTOM")]
    UnknownSide { line: usize, value: String },
    #[error("line {line}: unterminated quoted token")]
    UnterminatedQuote { line: usize },
}

pub fn parse_placement_claims(input: &str) -> Result<Vec<IdfPlacementClaim>, IdfError> {
    let mut in_placement = false;
    let mut saw_section = false;
    let mut pending_package: Option<(usize, String, Option<String>, String)> = None;
    let mut claims = Vec::new();

    for (idx, raw_line) in input.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let upper = line.to_ascii_uppercase();
        if upper == ".PLACEMENT" {
            in_placement = true;
            saw_section = true;
            continue;
        }
        if upper == ".END_PLACEMENT" {
            if let Some((line, _, _, _)) = pending_package.take() {
                return Err(IdfError::MissingCoordinateRecord { line });
            }
            in_placement = false;
            continue;
        }
        if !in_placement {
            continue;
        }

        let tokens = tokenize(line, line_no)?;
        if tokens.is_empty() || tokens[0].starts_with('#') {
            continue;
        }

        if let Some((_, package, part_number, refdes)) = pending_package.take() {
            if tokens.len() < 6 {
                return Err(IdfError::MalformedCoordinateRecord {
                    line: line_no,
                    count: tokens.len(),
                });
            }
            claims.push(parse_coordinate_record(
                package,
                part_number,
                refdes,
                &tokens,
                line_no,
            )?);
            continue;
        }

        if tokens.len() >= 9 {
            claims.push(parse_compact_record(&tokens, line_no)?);
            continue;
        }

        if tokens.len() != 3 {
            return Err(IdfError::MalformedPackageRecord {
                line: line_no,
                count: tokens.len(),
            });
        }

        pending_package = Some((
            line_no,
            tokens[0].clone(),
            none_if_empty_or_placeholder(&tokens[1]),
            tokens[2].clone(),
        ));
    }

    if !saw_section {
        return Err(IdfError::MissingPlacementSection);
    }
    if let Some((line, _, _, _)) = pending_package {
        return Err(IdfError::MissingCoordinateRecord { line });
    }
    Ok(claims)
}

fn parse_compact_record(tokens: &[String], line: usize) -> Result<IdfPlacementClaim, IdfError> {
    parse_coordinate_record(
        tokens[0].clone(),
        none_if_empty_or_placeholder(&tokens[1]),
        tokens[2].clone(),
        &tokens[3..],
        line,
    )
}

fn parse_coordinate_record(
    package: String,
    part_number: Option<String>,
    refdes: String,
    tokens: &[String],
    line: usize,
) -> Result<IdfPlacementClaim, IdfError> {
    if tokens.len() < 6 {
        return Err(IdfError::MalformedCoordinateRecord {
            line,
            count: tokens.len(),
        });
    }

    // tokens[2] is the mounting offset (z); validated but unused on a 2D board.
    parse_f64(&tokens[2], line, "mounting offset")?;
    Ok(IdfPlacementClaim {
        refdes,
        package,
        part_number,
        pose: MechanicalPose {
            x_mm: parse_f64(&tokens[0], line, "x")?,
            y_mm: parse_f64(&tokens[1], line, "y")?,
            rotation_deg: parse_f64(&tokens[3], line, "rotation")?,
            side: parse_side(&tokens[4], line)?,
        },
        mcad_owned: tokens[5].eq_ignore_ascii_case("MCAD"),
    })
}

fn none_if_empty_or_placeholder(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == "-" || trimmed == "~" {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn parse_f64(value: &str, line: usize, field: &'static str) -> Result<f64, IdfError> {
    // Reject NaN/inf: `"NaN".parse::<f64>()` succeeds but would corrupt pose math.
    match value.parse::<f64>() {
        Ok(parsed) if parsed.is_finite() => Ok(parsed),
        _ => Err(IdfError::InvalidNumber {
            line,
            field,
            value: value.to_owned(),
        }),
    }
}

fn parse_side(value: &str, line: usize) -> Result<BoardSide, IdfError> {
    match value.to_ascii_uppercase().as_str() {
        "TOP" | "T" => Ok(BoardSide::Top),
        "BOTTOM" | "BOT" | "B" => Ok(BoardSide::Bottom),
        _ => Err(IdfError::UnknownSide {
            line,
            value: value.to_owned(),
        }),
    }
}

fn tokenize(line: &str, line_no: usize) -> Result<Vec<String>, IdfError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    let mut in_quote = false;
    let mut token_started = false;

    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                in_quote = !in_quote;
                token_started = true;
            }
            '#' if !in_quote => break,
            c if c.is_whitespace() && !in_quote => {
                if token_started {
                    tokens.push(std::mem::take(&mut current));
                    token_started = false;
                }
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
            }
            c => {
                current.push(c);
                token_started = true;
            }
        }
    }

    if in_quote {
        return Err(IdfError::UnterminatedQuote { line: line_no });
    }
    if token_started {
        tokens.push(current);
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spec_placement_record_pairs() {
        let input = r#"
.PLACEMENT
"USB_C" "Molex" J1
10.5 20.25 0 90 TOP MCAD
.END_PLACEMENT
"#;
        let claims = parse_placement_claims(input).unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].refdes, "J1");
        assert_eq!(claims[0].package, "USB_C");
        assert_eq!(claims[0].part_number.as_deref(), Some("Molex"));
        assert_eq!(claims[0].pose.side, BoardSide::Top);
        assert!(claims[0].mcad_owned);
    }

    #[test]
    fn preserves_empty_quoted_part_number() {
        let input = r#"
.PLACEMENT
"USB_C" "" J1
10.5 20.25 0 90 TOP MCAD
.END_PLACEMENT
"#;
        let claims = parse_placement_claims(input).unwrap();
        assert_eq!(claims[0].part_number, None);
    }

    #[test]
    fn tolerates_compact_placement_records() {
        let input = r#"
.PLACEMENT
"USB_C" "Molex" J1 10.5 20.25 0 90 TOP MCAD
.END_PLACEMENT
"#;
        let claims = parse_placement_claims(input).unwrap();
        assert_eq!(claims.len(), 1);
        assert!(claims[0].mcad_owned);
    }
}
