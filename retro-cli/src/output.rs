//! Where a command writes its report or its trace.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result};

/// A buffered writer to `path`, or to standard output when there is none.
pub fn open_output<P: AsRef<Path>>(path: Option<P>) -> Result<Box<dyn Write>> {
    match path {
        Some(path) => {
            let path = path.as_ref();
            let file = File::create(path)
                .with_context(|| format!("failed to create output file: {}", path.display()))?;
            Ok(Box::new(BufWriter::new(file)))
        }
        None => Ok(Box::new(BufWriter::new(std::io::stdout()))),
    }
}
