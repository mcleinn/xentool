use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::Serialize;
use std::fs::{File, create_dir_all};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::mpe::{DecodedEvent, EventRecord};

pub struct JsonlLogger {
    path: PathBuf,
    writer: BufWriter<File>,
}

impl JsonlLogger {
    pub fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            create_dir_all(parent)?;
        }
        let file = File::create(&path)?;
        Ok(Self {
            path,
            writer: BufWriter::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&mut self, decoded: &DecodedEvent, log_raw: bool) -> Result<()> {
        for record in decoded.records(log_raw) {
            serde_json::to_writer(&mut self.writer, &record)?;
            self.writer.write_all(b"\n")?;
        }
        self.writer.flush().context("failed to flush log")?;
        Ok(())
    }
}

pub fn default_log_path() -> PathBuf {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    let stamp = now
        .format(&Rfc3339)
        .unwrap_or_else(|_| "session".to_string());
    let project_dirs = ProjectDirs::from("", "", "xentool");
    let base = project_dirs
        .map(|dirs| dirs.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("logs"));
    base.join("logs")
        .join(format!("{}.jsonl", sanitize_filename(&stamp)))
}

fn sanitize_filename(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            ':' | '/' | '\\' => '-',
            _ => ch,
        })
        .collect()
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
struct LogEnvelope<'a> {
    #[serde(flatten)]
    record: &'a EventRecord,
}
