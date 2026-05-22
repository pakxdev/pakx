//! `pakx pack [<path>]` — build a gzipped tarball of a local skill bundle.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::pack::pack_dir;
use crate::ui;

#[derive(Debug, Clone, Args)]
pub struct PackArgs {
    /// Source directory containing `SKILL.md`. Defaults to cwd.
    pub source: Option<PathBuf>,

    /// Where to write the tarball. Defaults to cwd.
    #[arg(short = 'o', long = "out")]
    pub out: Option<PathBuf>,
}

#[allow(clippy::unused_async)] // matches the other commands::*::run signatures
pub async fn run(args: PackArgs) -> Result<()> {
    let src = args.source.unwrap_or_else(|| PathBuf::from("."));
    let out_dir = args.out.unwrap_or_else(|| PathBuf::from("."));
    let result = pack_dir(&src, &out_dir)?;
    eprintln!(
        "{} packed {} -> {} ({} bytes)",
        ui::glyph_ok_err(),
        ui::success_err(&format!(
            "{}@{}",
            result.manifest.name, result.manifest.version
        )),
        result.tarball_path.display(),
        result.bytes.len(),
    );
    Ok(())
}
