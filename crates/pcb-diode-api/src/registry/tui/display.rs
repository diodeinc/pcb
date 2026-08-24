use colored::Colorize;

use crate::SearchHit;

pub struct RegistryModuleDisplay {
    pub path: String,
    pub registry: String,
    pub version: String,
    pub description: String,
}

impl RegistryModuleDisplay {
    pub fn from_hit(hit: &crate::RegistryModuleHit) -> Self {
        Self {
            path: registry_relative_path(&hit.url, &hit.registry.registry_url),
            registry: hit.registry.display_name(),
            version: hit.version.clone(),
            description: hit.description.clone(),
        }
    }

    pub fn to_cli_lines(&self) -> Vec<String> {
        vec![
            format!(
                "{} {} {}",
                self.path.blue(),
                format!("({})", self.version).yellow().dimmed(),
                format!("[{}]", self.registry).dimmed()
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
                Span::styled(
                    format!(" [{}]", self.registry),
                    base_style.fg(Color::DarkGray),
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
    pub registry: String,
    pub mpn: String,
    pub manufacturer: String,
    pub description: Option<String>,
}

impl RegistrySymbolDisplay {
    pub fn from_hit(hit: &crate::RegistrySymbolHit) -> Self {
        Self {
            path: registry_relative_path(&hit.url, &hit.registry.registry_url),
            registry: hit.registry.display_name(),
            mpn: hit.mpn.clone(),
            manufacturer: hit.manufacturer.clone(),
            description: hit.kicad_description.clone(),
        }
    }

    pub fn to_cli_lines(&self) -> Vec<String> {
        let mut lines = vec![
            self.path.green().to_string(),
            format!(
                "  {} {} {}",
                self.mpn,
                format!("· {}", self.manufacturer).dimmed(),
                format!("[{}]", self.registry).dimmed()
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
                Span::styled(
                    format!(" [{}]", self.registry),
                    base_style.fg(Color::DarkGray),
                ),
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

pub fn registry_relative_path(url: &str, registry_url: &str) -> String {
    let registry_url = registry_url.trim_end_matches('/');
    if let Some(rest) = url.strip_prefix(registry_url) {
        return rest.trim_start_matches('/').to_string();
    }

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
        let path = hit.url.replace(".kicad_sym:", "/");

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
