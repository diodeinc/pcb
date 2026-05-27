use anyhow::Result;
use std::path::Path;

use super::{Codemod, MigrateContext, rewrite_strings};

const ALIAS_MAPPINGS: &[(&str, &str)] = &[("@registry", "github.com/diodeinc/registry")];

/// Expand hardcoded aliases in .zen files (e.g., @registry -> github.com/diodeinc/registry)
pub struct AliasExpansion;

impl Codemod for AliasExpansion {
    fn apply(&self, _ctx: &MigrateContext, _path: &Path, content: &str) -> Result<Option<String>> {
        Ok(rewrite_strings(content, try_expand_alias))
    }
}

/// Try to expand an alias, returns None if no expansion needed
fn try_expand_alias(path_str: &str) -> Option<String> {
    for (alias, expansion) in ALIAS_MAPPINGS {
        if let Some(rest) = path_str.strip_prefix(alias)
            && (rest.is_empty() || rest.starts_with('/'))
        {
            return Some(format!("{}{}", expansion, rest));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_try_expand_alias() {
        assert_eq!(
            try_expand_alias("@registry/components/LED.zen"),
            Some("github.com/diodeinc/registry/components/LED.zen".to_string())
        );

        assert_eq!(
            try_expand_alias("@registry"),
            Some("github.com/diodeinc/registry".to_string())
        );

        assert_eq!(try_expand_alias("@stdlib/units.zen"), None);
        assert_eq!(try_expand_alias("./local.zen"), None);
        assert_eq!(try_expand_alias("@registryother/foo.zen"), None);
    }

    #[test]
    fn test_convert_file_with_registry_alias() -> Result<()> {
        let content = r#"load("@registry/components/LED.zen", "LED")
MyModule = Module("@registry/modules/Power.zen")
"#;

        let ctx = MigrateContext {
            workspace_root: PathBuf::from("/workspace"),
            repository: "github.com/test/repo".to_string(),
            repo_subpath: None,
        };
        let codemod = AliasExpansion;
        let result = codemod.apply(&ctx, Path::new("test.zen"), content)?;
        assert!(result.is_some());

        let updated = result.unwrap();
        assert!(updated.contains("\"github.com/diodeinc/registry/components/LED.zen\""));
        assert!(updated.contains("\"github.com/diodeinc/registry/modules/Power.zen\""));

        Ok(())
    }

    #[test]
    fn test_convert_file_no_aliases() -> Result<()> {
        let content = r#"load("@stdlib/units.zen", "Voltage")
MyModule = Module("./local.zen")
"#;

        let ctx = MigrateContext {
            workspace_root: PathBuf::from("/workspace"),
            repository: "github.com/test/repo".to_string(),
            repo_subpath: None,
        };
        let codemod = AliasExpansion;
        let result = codemod.apply(&ctx, Path::new("test.zen"), content)?;
        assert!(result.is_none());

        Ok(())
    }
}
