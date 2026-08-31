//! Committed frames, written out as image files.
//!
//! The bytes come from `render.rs`'s PPM query and reach the file unchanged.
//! Nothing here decides a pixel: it issues the query, takes the string back
//! and writes it.

use std::path::{Path, PathBuf};

use crate::client::Db;
use crate::render;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] crate::client::Error),
    #[error("writing {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Creates `dir`, so an unwritable path fails before a run starts rather
/// than at its first committed frame.
pub fn prepare(dir: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(dir).map_err(|source| Error::Write {
        path: dir.to_owned(),
        source,
    })
}

/// Writes the latest committed frame as a binary PPM and returns the path.
///
/// Reads the newest `frames_out` row, so the caller runs
/// [`render::frame_readout_sql`] first. Naming the file after `frame_no`
/// rather than a counter means a resumed run rewrites the frame it renders
/// again instead of renumbering every frame after it.
pub async fn write_committed(
    db: &Db,
    database: &str,
    dir: &Path,
    frame_no: u32,
) -> Result<PathBuf, Error> {
    let ppm: bytes::Bytes = db
        .fetch_one(&render::ppm_render_sql(
            database,
            render::FB_WIDTH,
            render::FB_HEIGHT,
        ))
        .await?;
    let path = dir.join(format!("frame-{frame_no:05}.ppm"));
    std::fs::write(&path, &ppm).map_err(|source| Error::Write {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}
