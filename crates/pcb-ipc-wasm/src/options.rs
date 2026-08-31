use pcb_ipc2581_tools::commands::cpl::CplSideFilter;
use pcb_ipc2581_tools::{LayoutTarget, ViewMode};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportOptions {
    pub name: Option<String>,
    pub validate: bool,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "format",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ExportOptions {
    Ipc2581 {
        #[serde(default)]
        mode: Option<ViewMode>,
    },
    Gerber {
        #[serde(default)]
        layout_target: LayoutTarget,
        #[serde(default)]
        zip: bool,
    },
    Svg {
        layer: String,
        #[serde(default)]
        layout_target: LayoutTarget,
    },
    Png {
        layer: String,
        #[serde(default)]
        layout_target: LayoutTarget,
    },
    Dxf {
        #[serde(default)]
        layout_target: LayoutTarget,
    },
    Bom {},
    Cpl {
        #[serde(default)]
        side: CplSideFilter,
        #[serde(default)]
        exclude_dnp: bool,
    },
    Ict {
        #[serde(default)]
        side: CplSideFilter,
    },
    Html {},
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum PdkInput {
    Builtin(String),
    Toml(TextInput),
}

impl Default for PdkInput {
    fn default() -> Self {
        Self::Builtin("standard".to_owned())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextInput {
    pub source: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct DfmOptions {
    pub pdk: PdkInput,
    pub waivers: Option<TextInput>,
    pub layout_target: LayoutTarget,
    pub generated_at: Option<String>,
}
