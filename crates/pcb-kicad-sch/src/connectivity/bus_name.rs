//! KiCad 10 bus labels, after KiCad's static net-name unescaping.
//!
//! Grammar reference: `NET_SETTINGS::{ParseBusVector, ParseBusGroup}` in
//! https://raw.githubusercontent.com/KiCad/kicad-source-mirror/10.0.6/common/project/net_settings.cpp
//! Alias lookup and member ordering follow `SCH_CONNECTION::ConfigureFromLabel` in
//! https://raw.githubusercontent.com/KiCad/kicad-source-mirror/10.0.6/eeschema/sch_connection.cpp

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail, ensure};

const MAX_LEAVES: usize = 65_536;
// Also bound empty expansions and recursion, neither of which consumes leaves.
const MAX_STEPS: usize = MAX_LEAVES * 4;
const MAX_DEPTH: usize = 128;

#[derive(Debug, Clone)]
pub(super) struct BusSchema {
    pub group: bool,
    pub members: Vec<BusMember>,
}

#[derive(Debug, Clone)]
pub(super) struct BusMember {
    pub name: String,
    /// KiCad's LocalName: the leaf name without its group's prefix.
    pub local_name: String,
}

pub(super) fn parse(
    name: &str,
    aliases: &BTreeMap<String, Vec<String>>,
) -> Result<Option<BusSchema>> {
    let Some(bus) = syntax(name)? else {
        return Ok(None);
    };
    let group = matches!(bus, Syntax::Group { .. });
    let mut expansion = Expansion {
        aliases,
        active_aliases: BTreeSet::new(),
        names: Vec::new(),
        steps: 0,
    };
    expansion.bus(bus, "", 0)?;
    Ok(Some(BusSchema {
        group,
        members: expansion.names,
    }))
}

enum Syntax {
    Vector {
        prefix: String,
        suffix: String,
        first: u64,
        last: u64,
    },
    Group {
        prefix: String,
        members: Vec<String>,
    },
}

fn syntax(name: &str) -> Result<Option<Syntax>> {
    let chars: Vec<_> = name.chars().collect();
    if let Some(vector) = vector(&chars)? {
        return Ok(Some(vector));
    }
    Ok(group(&chars))
}

fn formatting(c: char) -> bool {
    matches!(c, '~' | '_' | '^')
}

/// Read the prefix through the opening range/list delimiter. Quotes and escaped
/// spaces are lexical quoting, not part of the resulting electrical name.
fn prefix(chars: &[char], vector: bool) -> Option<(String, usize, usize)> {
    let mut name = String::new();
    let mut depth = 0usize;
    let mut quoted = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() && (quoted || chars[i + 1] == ' ') {
            name.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '"' && !escaped(chars, i) {
            quoted = !quoted;
        } else if quoted {
            name.push(c);
        } else {
            match c {
                '{' if i > 0 && formatting(chars[i - 1]) => {
                    depth += 1;
                    name.push(c);
                }
                // static_net_text has already rejected live expressions and
                // unquoted escaped ones. Their literal braces are not a bus.
                '{' if i > 0 && matches!(chars[i - 1], '$' | '@') => return None,
                '{' if !vector && depth == 0 => return Some((name, i + 1, depth)),
                '{' | ' ' | ']' => return None,
                '[' if vector => return Some((name, i + 1, depth)),
                '[' => return None,
                '}' => {
                    depth = depth.checked_sub(1)?;
                    name.push(c);
                }
                _ => name.push(c),
            }
        }
        i += 1;
    }
    None
}

fn escaped(chars: &[char], i: usize) -> bool {
    chars[..i].iter().rev().take_while(|&&c| c == '\\').count() % 2 == 1
}

fn vector(chars: &[char]) -> Result<Option<Syntax>> {
    let Some((mut prefix, mut i, mut depth)) = prefix(chars, true) else {
        return Ok(None);
    };
    let mut wraps_name = false;
    if depth > 0
        && let Some(start) = prefix.rfind('{')
        && prefix[..start].chars().next_back().is_some_and(formatting)
    {
        if start + 1 == prefix.len() {
            // D_{[0..1]} -> D0, D1; ~{D[0..1]} -> ~{D0}, ~{D1}.
            prefix.truncate(start - 1);
        } else {
            wraps_name = true;
        }
    }

    let first_start = i;
    while i < chars.len() && chars[i].is_ascii_digit() {
        i += 1;
    }
    if i == first_start || chars.get(i..i + 2) != Some(&['.', '.'][..]) {
        return Ok(None);
    }
    let first: String = chars[first_start..i].iter().collect();
    i += 2;
    let last_start = i;
    while i < chars.len() && chars[i].is_ascii_digit() {
        i += 1;
    }
    if i == last_start || chars.get(i) != Some(&']') {
        return Ok(None);
    }
    let last: String = chars[last_start..i].iter().collect();
    i += 1;

    let mut suffix = String::new();
    for &c in &chars[i..] {
        match c {
            '}' if depth > 0 => {
                depth -= 1;
                if wraps_name {
                    suffix.push(c);
                }
            }
            '+' | '-' | 'P' | 'N' => suffix.push(c),
            _ => return Ok(None),
        }
    }
    if depth != 0 || first.trim_start_matches('0') == last.trim_start_matches('0') {
        return Ok(None);
    }
    // Validate all grammar before reporting a numeric overflow as excessive
    // expansion rather than accidentally treating a malformed label as a bus.
    let (Ok(first), Ok(last)) = (first.parse::<u64>(), last.parse::<u64>()) else {
        bail!("bus vector range exceeds supported unsigned integer bounds");
    };
    Ok(Some(Syntax::Vector {
        prefix,
        suffix,
        first: first.min(last),
        last: first.max(last),
    }))
}

fn group(chars: &[char]) -> Option<Syntax> {
    let (prefix, mut i, mut depth) = prefix(chars, false)?;
    let mut members = Vec::new();
    let mut member = String::new();
    let mut quoted = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() && (quoted || chars[i + 1] == ' ') {
            member.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '"' && !escaped(chars, i) {
            quoted = !quoted;
        } else if quoted {
            member.push(c);
        } else {
            match c {
                '{' if i > 0 && formatting(chars[i - 1]) => {
                    depth += 1;
                    member.push(c);
                }
                // Direct nested groups are not KiCad bus-group syntax.
                '{' => return None,
                '}' if depth > 0 => {
                    depth -= 1;
                    member.push(c);
                }
                '}' => {
                    if !member.is_empty() {
                        members.push(member);
                    }
                    // Like ParseBusGroup, the first group-closing brace ends
                    // the label; text following it is ignored.
                    return Some(Syntax::Group { prefix, members });
                }
                ' ' | ',' => {
                    if !member.is_empty() {
                        members.push(std::mem::take(&mut member));
                    }
                }
                _ => member.push(c),
            }
        }
        i += 1;
    }
    None
}

fn group_prefix(prefix: &str) -> String {
    if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}.")
    }
}

struct Expansion<'a> {
    aliases: &'a BTreeMap<String, Vec<String>>,
    active_aliases: BTreeSet<String>,
    names: Vec<BusMember>,
    steps: usize,
}

impl Expansion<'_> {
    fn step(&mut self, depth: usize) -> Result<()> {
        self.steps += 1;
        ensure!(
            self.steps <= MAX_STEPS && depth <= MAX_DEPTH,
            "excessive bus expansion: work or nesting limit exceeded"
        );
        Ok(())
    }

    fn leaf(&mut self, name: String, prefix: &str) -> Result<()> {
        ensure!(
            self.names.len() < MAX_LEAVES,
            "bus expansion exceeds {MAX_LEAVES} leaves"
        );
        self.names.push(BusMember {
            name: format!("{prefix}{name}"),
            local_name: name,
        });
        Ok(())
    }

    fn name(&mut self, name: &str, scalar: &str, prefix: &str, depth: usize) -> Result<()> {
        self.step(depth)?;
        if let Some(bus) = syntax(name)? {
            self.bus(bus, prefix, depth)
        } else {
            self.leaf(scalar.to_owned(), prefix)
        }
    }

    fn bus(&mut self, bus: Syntax, parent_prefix: &str, depth: usize) -> Result<()> {
        match bus {
            Syntax::Vector {
                prefix,
                suffix,
                first,
                last,
            } => {
                // Subtract before adding one, so even [0..u64::MAX] is safe.
                ensure!(
                    last - first < (MAX_LEAVES - self.names.len()) as u64,
                    "bus expansion exceeds {MAX_LEAVES} leaves"
                );
                for index in first..=last {
                    self.leaf(format!("{prefix}{index}{suffix}"), parent_prefix)?;
                }
            }
            Syntax::Group { prefix, members } => {
                // ConfigureFromLabel starts a new prefix for a nested group;
                // unlike vectors, it does not inherit the enclosing prefix.
                let prefix = group_prefix(&prefix);
                for member in members {
                    self.step(depth)?;
                    // Only group members undergo alias lookup. A bare alias
                    // encountered inside an alias definition remains a scalar.
                    if let Some(alias) = self.aliases.get(&member) {
                        ensure!(
                            self.active_aliases.insert(member.clone()),
                            "recursive bus alias cycle involving {member:?}"
                        );
                        for name in alias {
                            self.name(name, name, &prefix, depth + 1)?;
                        }
                        self.active_aliases.remove(&member);
                    } else {
                        // Quoted/escaped spaces must survive recursive parsing,
                        // but are not part of the scalar's electrical spelling.
                        let escaped = member.replace(' ', "\\ ");
                        self.name(&escaped, &member, &prefix, depth + 1)?;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(name: &str) -> Vec<String> {
        parse(name, &BTreeMap::new())
            .unwrap()
            .unwrap()
            .members
            .into_iter()
            .map(|member| member.name)
            .collect()
    }

    fn aliases(entries: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        entries
            .iter()
            .map(|(name, members)| {
                (
                    (*name).to_owned(),
                    members.iter().map(|name| (*name).to_owned()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn vectors_have_ascending_ordinal_order_without_zero_padding() {
        let bus = parse("D[7..4]", &BTreeMap::new()).unwrap().unwrap();
        assert!(!bus.group);
        assert_eq!(names("D[7..4]"), ["D4", "D5", "D6", "D7"]);
        assert_eq!(names("D[004..007]"), names("D[7..4]"));
        assert_eq!(names("[2..0]"), ["0", "1", "2"]);
        assert_eq!(names("D[9..10]+-PN"), ["D9+-PN", "D10+-PN"]);
        assert!(bus.members.iter().all(|m| m.name == m.local_name));
    }

    #[test]
    fn groups_keep_member_order_and_local_names() {
        let bus = parse("Foo{SDA, SCL D[1..0]}", &BTreeMap::new())
            .unwrap()
            .unwrap();
        assert!(bus.group);
        assert_eq!(
            bus.members
                .iter()
                .map(|m| (m.name.as_str(), m.local_name.as_str()))
                .collect::<Vec<_>>(),
            [
                ("Foo.SDA", "SDA"),
                ("Foo.SCL", "SCL"),
                ("Foo.D0", "D0"),
                ("Foo.D1", "D1")
            ]
        );
        assert_eq!(
            names("{ B[5..4], A[1..0],, B4 }"),
            ["B4", "B5", "A0", "A1", "B4"]
        );
        assert!(names("Empty{}").is_empty());
        assert_eq!(names("Foo{A}ignored"), ["Foo.A"]);
    }

    #[test]
    fn formatting_is_part_of_the_name_unless_it_decorates_the_range() {
        assert_eq!(names("I^{2}C[0..1]"), ["I^{2}C0", "I^{2}C1"]);
        assert_eq!(names("~{D[0..1]}"), ["~{D0}", "~{D1}"]);
        for marker in ['~', '_', '^'] {
            assert_eq!(names(&format!("D{marker}{{[0..1]}}")), ["D0", "D1"]);
        }
        assert_eq!(
            names("I^{2}C{~{SDA} S_{CL} D_{[0..1]}}"),
            ["I^{2}C.~{SDA}", "I^{2}C.S_{CL}", "I^{2}C.D0", "I^{2}C.D1"]
        );
        assert_eq!(names("µ[1..2]"), ["µ1", "µ2"]);
    }

    #[test]
    fn quotes_and_escaped_spaces_preserve_electrical_spelling() {
        assert_eq!(names(r#""Data Bus"[1..2]"#), ["Data Bus1", "Data Bus2"]);
        assert_eq!(names(r"Data\ Bus[1..2]"), ["Data Bus1", "Data Bus2"]);
        assert_eq!(
            names(r#""My Bus"{"A B" C\ D "E F[0..1]" "x,y" "say\"hi"}"#),
            [
                "My Bus.A B",
                "My Bus.C D",
                "My Bus.E F0",
                "My Bus.E F1",
                "My Bus.x,y",
                "My Bus.say\"hi"
            ]
        );
        assert_eq!(names(r#"{"Inner{A,B}"}"#), ["Inner.A", "Inner.B"]);
    }

    #[test]
    fn aliases_are_only_resolved_at_group_member_positions() {
        let aliases = aliases(&[
            ("SPI", &["CLK", "D[3..2]", "OTHER", "Inner{PAIR}"]),
            ("OTHER", &["not expanded"]),
            ("PAIR", &["P", "N"]),
        ]);
        assert!(parse("SPI", &aliases).unwrap().is_none());
        let bus = parse("Port{SPI SPI}", &aliases).unwrap().unwrap();
        let expected = [
            ("Port.CLK", "CLK"),
            ("Port.D2", "D2"),
            ("Port.D3", "D3"),
            ("Port.OTHER", "OTHER"),
            ("Inner.P", "P"),
            ("Inner.N", "N"),
        ];
        assert_eq!(bus.members.len(), expected.len() * 2);
        for (member, &(name, local_name)) in bus.members.iter().zip(expected.iter().cycle()) {
            assert_eq!(member.local_name, local_name);
            assert_eq!(member.name, name);
        }
    }

    #[test]
    fn malformed_bus_labels_remain_scalars() {
        for name in [
            "SDA",
            "~{RESET}",
            "${SIGNAL}",
            "@{SIGNAL}",
            "D[2..2]",
            "D[02..2]",
            "D[-1..2]",
            "D[+1..2]",
            "D[0..]",
            "D[..2]",
            "D[0..2",
            "D[0.2]",
            "D[0...2]",
            "D[0..2]x",
            "D[0..2] ",
            "D [0..2]",
            "D[0..2]}",
            "D_{[0..2]",
            "Foo {A B}",
            "Foo{A",
            "Foo{Inner{A B}}",
            "Foo{~{A}",
            "Foo{\"A}",
            "\"D[0..2]",
        ] {
            assert!(parse(name, &BTreeMap::new()).unwrap().is_none(), "{name}");
        }
        assert_eq!(names("{D[2..2] D[0..2]x}"), ["D[2..2]", "D[0..2]x"]);
    }

    #[test]
    fn alias_cycles_are_explicit_errors_but_bare_aliases_are_not_cycles() {
        let recursive = aliases(&[("A", &["{B}"]), ("B", &["Named{A}"])]);
        assert!(
            parse("{A}", &recursive)
                .unwrap_err()
                .to_string()
                .contains("cycle")
        );
        let bare = aliases(&[("A", &["A"])]);
        let bus = parse("{A}", &bare).unwrap().unwrap();
        assert_eq!(bus.members[0].name, "A");
    }

    #[test]
    fn expansion_limits_never_silently_truncate() {
        assert_eq!(names("D[0..65535]").len(), MAX_LEAVES);
        assert_eq!(
            names("D[18446744073709551614..18446744073709551615]"),
            ["D18446744073709551614", "D18446744073709551615"]
        );
        for name in [
            "D[0..65536]",
            "{D[0..65535] X}",
            "D[0..18446744073709551615]",
            "D[0..18446744073709551616]",
        ] {
            assert!(parse(name, &BTreeMap::new()).is_err(), "{name}");
        }
        let aliases = aliases(&[("BIG", &["D[0..32767]"])]);
        assert_eq!(
            parse("{BIG BIG}", &aliases).unwrap().unwrap().members.len(),
            MAX_LEAVES
        );
        assert!(parse("{BIG BIG X}", &aliases).is_err());
    }

    #[test]
    fn deeply_nested_aliases_fail_without_exhausting_the_stack() {
        let mut aliases = BTreeMap::new();
        for i in 0..MAX_DEPTH + 2 {
            aliases.insert(format!("A{i}"), vec![format!("{{A{}}}", i + 1)]);
        }
        assert!(
            parse("{A0}", &aliases)
                .unwrap_err()
                .to_string()
                .contains("nesting limit")
        );
    }

    #[test]
    fn empty_alias_expansions_also_have_a_work_limit() {
        let mut aliases = BTreeMap::new();
        aliases.insert("A0".to_owned(), vec![]);
        for i in 1..20 {
            aliases.insert(format!("A{i}"), vec![format!("{{A{} A{}}}", i - 1, i - 1)]);
        }
        assert!(
            parse("{A19}", &aliases)
                .unwrap_err()
                .to_string()
                .contains("excessive")
        );
    }
}
