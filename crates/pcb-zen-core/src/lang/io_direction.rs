use allocative::Allocative;
use serde::{Deserialize, Serialize};
use starlark::values::{Freeze, Trace};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Trace, Freeze, Allocative)]
#[serde(rename_all = "snake_case")]
pub enum IoDirection {
    Input,
    Output,
}

impl std::str::FromStr for IoDirection {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "input" => Ok(Self::Input),
            "output" => Ok(Self::Output),
            _ => anyhow::bail!("io() direction must be \"input\" or \"output\""),
        }
    }
}

impl IoDirection {
    pub fn parse_optional(direction: Option<&str>) -> anyhow::Result<Option<Self>> {
        direction.map(str::parse).transpose()
    }
}

impl std::fmt::Display for IoDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input => write!(f, "input"),
            Self::Output => write!(f, "output"),
        }
    }
}
