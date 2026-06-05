use anyhow::{Context, Result};
use std::path::Path;

use super::{Codemod, MigrateContext, rewrite_strings};

/// Convert repository URLs that point back into this workspace to file-relative paths.
pub struct LocalUrlPaths;

impl Codemod for LocalUrlPaths {
    fn apply(
        &self,
        ctx: &MigrateContext,
        zen_file: &Path,
        content: &str,
    ) -> Result<Option<String>> {
        let zen_dir = zen_file.parent().context("Zen file has no parent")?;
        Ok(rewrite_strings(content, |s| {
            local_url_to_relative(ctx, zen_dir, s)
        }))
    }
}

fn local_url_to_relative(ctx: &MigrateContext, zen_dir: &Path, path: &str) -> Option<String> {
    let prefix = match &ctx.repo_subpath {
        Some(repo_subpath) => format!(
            "{}/{}/",
            ctx.repository,
            repo_subpath.to_string_lossy().replace('\\', "/")
        ),
        None => format!("{}/", ctx.repository),
    };
    let rel = path.strip_prefix(&prefix)?;
    let target = ctx.workspace_root.join(rel);
    if !target.exists() {
        return None;
    }
    let relative = pathdiff::diff_paths(&target, zen_dir)?;
    let relative = relative.to_string_lossy().replace('\\', "/");
    if relative.starts_with("..") || relative.starts_with('/') {
        Some(relative)
    } else {
        Some(format!("./{}", relative))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn converts_local_repository_url_to_relative_path() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let workspace_root = temp.path().to_path_buf();
        let module_dir = workspace_root.join("modules");
        let component_dir = workspace_root.join("components/Vendor/Part");
        fs::create_dir_all(&module_dir)?;
        fs::create_dir_all(&component_dir)?;
        fs::write(component_dir.join("Part.zen"), "")?;

        let ctx = MigrateContext {
            workspace_root,
            repository: "code.diode.computer/demo/b/DM0001".to_string(),
            repo_subpath: None,
        };

        assert_eq!(
            local_url_to_relative(
                &ctx,
                &module_dir,
                "code.diode.computer/demo/b/DM0001/components/Vendor/Part/Part.zen"
            ),
            Some("../components/Vendor/Part/Part.zen".to_string())
        );

        Ok(())
    }
}
