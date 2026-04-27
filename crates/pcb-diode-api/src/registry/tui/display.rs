use colored::Colorize;

use crate::SearchHit;

pub struct RegistryModuleDisplay {
    pub path: String,
    pub version: String,
    pub description: String,
}

impl RegistryModuleDisplay {
    pub fn from_hit(hit: &crate::RegistryModuleHit) -> Self {
        Self {
            path: registry_relative_path(&hit.url),
            version: hit.version.clone(),
            description: hit.description.clone(),
        }
    }

    pub fn to_cli_lines(&self) -> Vec<String> {
        vec![
            format!(
                "{} {}",
                self.path.blue(),
                format!("({})", self.version).yellow().dimmed()
            ),
            format!("  {}", self.description.dimmed()),
        ]
    }

    pub fn to_tui_lines(
        &self,
        is_selected: bool,
        base_style: ratatui::style::Style,
        prefix_style: ratatui::style::Style,
    ) -> Vec<ratatui::text::Line<'static>> {
        use ratatui::style::{Color, Modifier};
        use ratatui::text::{Line, Span};

        let prefix = if is_selected { "▌" } else { " " };
        let path_style = if is_selected {
            base_style.fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            base_style.fg(Color::Blue)
        };
        vec![
            Line::from(vec![
                Span::styled(prefix.to_string(), prefix_style),
                Span::styled(" ".to_string(), base_style),
                Span::styled(self.path.clone(), path_style),
                Span::styled(
                    format!(" ({})", self.version),
                    base_style.fg(Color::Yellow).add_modifier(Modifier::DIM),
                ),
            ]),
            Line::from(vec![
                Span::styled(prefix.to_string(), prefix_style),
                Span::styled("   ".to_string(), base_style),
                Span::styled(self.description.clone(), base_style.fg(Color::DarkGray)),
            ]),
        ]
    }
}

pub struct RegistrySymbolDisplay {
    pub path: String,
    pub mpn: String,
    pub manufacturer: String,
    pub description: Option<String>,
}

impl RegistrySymbolDisplay {
    pub fn from_hit(hit: &crate::RegistrySymbolHit) -> Self {
        Self {
            path: registry_relative_path(&hit.url),
            mpn: hit.mpn.clone(),
            manufacturer: hit.manufacturer.clone(),
            description: hit.kicad_description.clone(),
        }
    }

    pub fn to_cli_lines(&self) -> Vec<String> {
        let mut lines = vec![
            self.path.green().to_string(),
            format!(
                "  {} {}",
                self.mpn,
                format!("· {}", self.manufacturer).dimmed()
            ),
        ];
        if let Some(description) = self
            .description
            .as_deref()
            .filter(|description| !description.trim().is_empty())
        {
            lines.push(format!("  {}", description.dimmed()));
        }
        lines
    }

    pub fn to_tui_lines(
        &self,
        is_selected: bool,
        base_style: ratatui::style::Style,
        prefix_style: ratatui::style::Style,
    ) -> Vec<ratatui::text::Line<'static>> {
        use ratatui::style::{Color, Modifier};
        use ratatui::text::{Line, Span};

        let prefix = if is_selected { "▌" } else { " " };
        let path_style = if is_selected {
            base_style.fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            base_style.fg(Color::Green)
        };
        let mut lines = vec![
            Line::from(vec![
                Span::styled(prefix.to_string(), prefix_style),
                Span::styled(" ".to_string(), base_style),
                Span::styled(self.path.clone(), path_style),
            ]),
            Line::from(vec![
                Span::styled(prefix.to_string(), prefix_style),
                Span::styled("   ".to_string(), base_style),
                Span::styled(self.mpn.clone(), base_style.fg(Color::Gray)),
                Span::styled(" · ".to_string(), base_style.fg(Color::DarkGray)),
                Span::styled(self.manufacturer.clone(), base_style.fg(Color::DarkGray)),
            ]),
        ];
        let description = self
            .description
            .as_deref()
            .filter(|description| !description.trim().is_empty())
            .unwrap_or_default();
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), prefix_style),
            Span::styled("   ".to_string(), base_style),
            Span::styled(description.to_string(), base_style.fg(Color::DarkGray)),
        ]));
        lines
    }
}

pub fn registry_relative_path(url: &str) -> String {
    url.split('/').skip(3).collect::<Vec<_>>().join("/")
}

/// Formatted display of a KiCad symbol search result (shared between TUI and CLI)
pub struct KicadSymbolDisplay {
    pub path: String,
    pub line2_parts: Vec<(String, bool)>,
    pub line3: Option<String>,
}

impl KicadSymbolDisplay {
    pub fn from_hit(hit: &SearchHit) -> Self {
        let path = hit
            .url
            .strip_prefix("@kicad-symbols/")
            .unwrap_or(&hit.url)
            .replace(".kicad_sym:", "/");

        let mut line2_parts = Vec::new();
        if let Some(mpn) = hit.mpn.as_deref() {
            line2_parts.push((mpn.to_string(), false));
        } else {
            line2_parts.push((hit.name.clone(), false));
        }
        if let Some(manufacturer) = hit
            .manufacturer
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            line2_parts.push((" · ".to_string(), true));
            line2_parts.push((manufacturer.to_string(), true));
        }

        Self {
            path,
            line2_parts,
            line3: hit.short_description.clone(),
        }
    }

    pub fn to_cli_lines(&self) -> Vec<String> {
        let line1 = self.path.cyan().to_string();
        let line2 = format!(
            "  {}",
            self.line2_parts
                .iter()
                .map(|(text, dimmed)| {
                    if *dimmed {
                        text.dimmed().to_string()
                    } else {
                        text.clone()
                    }
                })
                .collect::<String>()
        );

        let mut lines = vec![line1, line2];
        if let Some(line3) = &self.line3 {
            lines.push(format!("  {}", line3.dimmed()));
        }
        lines
    }

    pub fn to_tui_lines(
        &self,
        is_selected: bool,
        base_style: ratatui::style::Style,
        prefix_style: ratatui::style::Style,
    ) -> Vec<ratatui::text::Line<'static>> {
        use ratatui::style::{Color, Modifier};
        use ratatui::text::{Line, Span};

        let prefix = if is_selected { "▌" } else { " " };
        let path_style = if is_selected {
            base_style.fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            base_style.fg(Color::Cyan)
        };

        let line1 = Line::from(vec![
            Span::styled(prefix.to_string(), prefix_style),
            Span::styled(" ".to_string(), base_style),
            Span::styled(self.path.clone(), path_style),
        ]);

        let mut line2_spans = vec![
            Span::styled(prefix.to_string(), prefix_style),
            Span::styled("   ".to_string(), base_style),
        ];
        for (text, dimmed) in &self.line2_parts {
            let style = if *dimmed {
                base_style.fg(Color::DarkGray)
            } else {
                base_style.fg(Color::Gray)
            };
            line2_spans.push(Span::styled(text.clone(), style));
        }

        let mut lines = vec![line1, Line::from(line2_spans)];
        if let Some(line3) = &self.line3 {
            lines.push(Line::from(vec![
                Span::styled(prefix.to_string(), prefix_style),
                Span::styled("   ".to_string(), base_style),
                Span::styled(line3.clone(), base_style.fg(Color::DarkGray)),
            ]));
        }
        lines
    }
}

/// Formatted display of a web component search result (shared between TUI and CLI)
pub struct WebComponentDisplay {
    pub path: String,
    pub source: Option<String>,
    pub has_ecad: bool,
    pub has_step: bool,
    pub has_datasheet: bool,
    pub mpn: String,
    pub manufacturer: Option<String>,
    pub package: Option<String>,
    pub description: Option<String>,
}

impl WebComponentDisplay {
    pub fn from_component(result: &crate::component::ComponentSearchResult) -> Self {
        use crate::component::sanitize_mpn_for_path;

        let mfr = result
            .manufacturer
            .as_deref()
            .map(sanitize_mpn_for_path)
            .unwrap_or_else(|| "unknown".to_string());
        let mpn_sanitized = sanitize_mpn_for_path(&result.part_number);
        let path = format!("components/{mfr}/{mpn_sanitized}");

        Self {
            path,
            source: result.source.clone(),
            has_ecad: result.model_availability.ecad_model,
            has_step: result.model_availability.step_model,
            has_datasheet: !result.datasheets.is_empty(),
            mpn: result.part_number.clone(),
            manufacturer: result.manufacturer.clone(),
            package: result.package_category.clone(),
            description: result.description.clone(),
        }
    }

    fn source_abbrev(&self) -> &'static str {
        self.source
            .as_deref()
            .and_then(|s| {
                let lower = s.to_lowercase();
                if lower.contains("cse") {
                    Some("C")
                } else if lower.contains("lcsc") {
                    Some("L")
                } else if lower.contains("ncti") {
                    Some("N")
                } else {
                    None
                }
            })
            .unwrap_or("?")
    }

    pub fn to_cli_lines(&self) -> Vec<String> {
        let line1 = self.path.green().to_string();
        let check = "✓".green().to_string();
        let cross = "✗".red().to_string();
        let src = self.source_abbrev();

        let mut line2_parts = vec![
            format!("[{src}]").dimmed().to_string(),
            " EDA:".to_string(),
            if self.has_ecad {
                check.clone()
            } else {
                cross.clone()
            },
            " STEP:".to_string(),
            if self.has_step {
                check.clone()
            } else {
                cross.clone()
            },
            " Datasheet:".to_string(),
            if self.has_datasheet { check } else { cross },
            " · ".dimmed().to_string(),
            self.mpn.yellow().to_string(),
        ];

        if let Some(mfr) = &self.manufacturer {
            line2_parts.push(" · ".dimmed().to_string());
            line2_parts.push(mfr.dimmed().to_string());
        }
        if let Some(pkg) = &self.package {
            line2_parts.push(" · ".dimmed().to_string());
            line2_parts.push(pkg.dimmed().to_string());
        }

        let line2 = format!("  {}", line2_parts.join(""));
        let line3 = format!("  {}", self.description.as_deref().unwrap_or("").dimmed());

        vec![line1, line2, line3]
    }

    fn source_color(&self) -> ratatui::style::Color {
        use ratatui::style::Color;

        self.source
            .as_deref()
            .map(|s| {
                let lower = s.to_lowercase();
                if lower.contains("cse") {
                    Color::Green
                } else if lower.contains("lcsc") {
                    Color::Yellow
                } else if lower.contains("ncti") {
                    Color::Cyan
                } else {
                    Color::DarkGray
                }
            })
            .unwrap_or(Color::DarkGray)
    }

    pub fn to_tui_lines(
        &self,
        is_selected: bool,
        base_style: ratatui::style::Style,
        prefix_style: ratatui::style::Style,
    ) -> Vec<ratatui::text::Line<'static>> {
        use ratatui::style::{Color, Modifier, Style};
        use ratatui::text::{Line, Span};

        let prefix = if is_selected { "▌" } else { " " };
        let path_style = if is_selected {
            base_style.fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            base_style.fg(Color::Green)
        };

        let line1 = Line::from(vec![
            Span::styled(prefix.to_string(), prefix_style),
            Span::styled(" ".to_string(), base_style),
            Span::styled(self.path.clone(), path_style),
        ]);

        let dim_bracket = Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM);
        let dim_src = Style::default()
            .fg(self.source_color())
            .add_modifier(Modifier::DIM);
        let label_style = Style::default().fg(Color::Gray);
        let check = Span::styled("✓".to_string(), Style::default().fg(Color::Green));
        let cross = Span::styled("✗".to_string(), Style::default().fg(Color::Red));

        let mut line2_spans = vec![
            Span::styled(prefix.to_string(), prefix_style),
            Span::styled("   [".to_string(), dim_bracket),
            Span::styled(self.source_abbrev().to_string(), dim_src),
            Span::styled("] ".to_string(), dim_bracket),
            Span::styled("EDA:".to_string(), label_style),
            if self.has_ecad {
                check.clone()
            } else {
                cross.clone()
            },
            Span::styled(" STEP:".to_string(), label_style),
            if self.has_step {
                check.clone()
            } else {
                cross.clone()
            },
            Span::styled(" Datasheet:".to_string(), label_style),
            if self.has_datasheet { check } else { cross },
            Span::styled(" · ".to_string(), Style::default().fg(Color::DarkGray)),
            Span::styled(self.mpn.clone(), base_style.fg(Color::Yellow)),
        ];

        if let Some(mfr) = &self.manufacturer {
            line2_spans.push(Span::styled(
                " · ".to_string(),
                Style::default().fg(Color::DarkGray),
            ));
            line2_spans.push(Span::styled(mfr.clone(), base_style.fg(Color::DarkGray)));
        }
        if let Some(pkg) = &self.package {
            line2_spans.push(Span::styled(
                " · ".to_string(),
                Style::default().fg(Color::DarkGray),
            ));
            line2_spans.push(Span::styled(pkg.clone(), base_style.fg(Color::DarkGray)));
        }

        let line2 = Line::from(line2_spans);
        let desc = self.description.as_deref().unwrap_or("");
        let line3 = Line::from(vec![
            Span::styled(prefix.to_string(), prefix_style),
            Span::styled("   ".to_string(), base_style),
            Span::styled(desc.to_string(), base_style.fg(Color::DarkGray)),
        ]);

        vec![line1, line2, line3]
    }
}
