use anyhow::{Context, Result, ensure};
use pcb_corpus::{Geometry, Status, html, load, replay};
use std::path::PathBuf;

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let output = PathBuf::from(
        args.next()
            .context("usage: pcb-corpus OUTPUT_DIRECTORY FIXTURE.json ...")?,
    );
    let mut paths = args.map(PathBuf::from).collect::<Vec<_>>();
    ensure!(!paths.is_empty(), "at least one fixture is required");
    paths.sort();
    let rows = paths
        .iter()
        .map(|path| match load(path, "geometry") {
            Ok(f) => {
                let report = replay(&f, &Geometry);
                (Some(f), report)
            }
            Err(report) => (None, *report),
        })
        .collect::<Vec<_>>();
    std::fs::create_dir_all(&output)?;
    std::fs::write(output.join("index.html"), html(&rows))?;
    std::fs::write(
        output.join("results.json"),
        serde_json::to_vec_pretty(&rows.iter().map(|(_, r)| r).collect::<Vec<_>>())?,
    )?;
    ensure!(
        rows.iter().all(|(_, r)| r.status == Status::Completed),
        "one or more replays did not complete; see results.json"
    );
    Ok(())
}
