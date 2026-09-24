//! Telemetry output: NDJSON lines in size-rotated files.
//!
//! `telemetry.ndjson` is the file being written; rotated files are
//! `telemetry.ndjson.1` (newest) up to `telemetry.ndjson.{max_files-1}`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use rules::LogRecord;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{info, warn};

pub const FILE_NAME: &str = "telemetry.ndjson";

pub struct RotatingWriter {
    dir: PathBuf,
    max_bytes: u64,
    max_files: u32,
    file: BufWriter<File>,
    size: u64,
}

impl RotatingWriter {
    /// Opens (appending to) the current file in `dir`, creating `dir` if needed.
    pub fn open(dir: &Path, max_bytes: u64, max_files: u32) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(FILE_NAME);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = file.metadata()?.len();
        Ok(RotatingWriter {
            dir: dir.to_owned(),
            max_bytes,
            max_files: max_files.max(1),
            file: BufWriter::new(file),
            size,
        })
    }

    fn path(&self, index: u32) -> PathBuf {
        if index == 0 {
            self.dir.join(FILE_NAME)
        } else {
            self.dir.join(format!("{FILE_NAME}.{index}"))
        }
    }

    /// Appends one line (a trailing newline is added). Rotates first if the
    /// line would push a non-empty file past `max_bytes`.
    pub fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        let len = line.len() as u64 + 1;
        if self.size > 0 && self.size + len > self.max_bytes {
            self.rotate()?;
        }
        self.file.write_all(line)?;
        self.file.write_all(b"\n")?;
        self.size += len;
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }

    /// Flushes and asks the OS to persist the data (used at shutdown).
    pub fn sync(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.file.get_ref().sync_data()
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        for i in (1..self.max_files).rev() {
            let src = self.path(i - 1);
            if src.exists() {
                let dst = self.path(i);
                if dst.exists() {
                    fs::remove_file(&dst)?;
                }
                fs::rename(&src, &dst)?;
            }
        }
        // Truncate covers max_files == 1, where nothing was renamed away.
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(self.path(0))?;
        self.file = BufWriter::new(file);
        self.size = 0;
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct OutputStats {
    pub records_written: u64,
    pub write_errors: u64,
}

/// Writes records until the channel closes, then flushes and syncs.
/// Runs on a blocking thread because it does synchronous file I/O.
pub fn spawn(
    mut writer: RotatingWriter,
    mut records: mpsc::Receiver<LogRecord>,
) -> JoinHandle<OutputStats> {
    tokio::task::spawn_blocking(move || {
        let mut stats = OutputStats::default();
        let mut write = |writer: &mut RotatingWriter, record: &LogRecord| {
            let result = serde_json::to_vec(record)
                .map_err(io::Error::other)
                .and_then(|line| writer.write_line(&line));
            match result {
                Ok(()) => stats.records_written += 1,
                Err(e) => {
                    stats.write_errors += 1;
                    if stats.write_errors == 1 || stats.write_errors % 1000 == 0 {
                        warn!(error = %e, errors = stats.write_errors, "failed to write telemetry");
                    }
                }
            }
        };
        while let Some(record) = records.blocking_recv() {
            write(&mut writer, &record);
            // Drain whatever else is queued, then flush once per batch.
            while let Ok(record) = records.try_recv() {
                write(&mut writer, &record);
            }
            if let Err(e) = writer.flush() {
                warn!(error = %e, "failed to flush telemetry output");
            }
        }
        if let Err(e) = writer.sync() {
            warn!(error = %e, "failed to sync telemetry output");
        }
        info!(
            records_written = stats.records_written,
            write_errors = stats.write_errors,
            "output closed"
        );
        stats
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(dir: &Path, name: &str) -> String {
        fs::read_to_string(dir.join(name)).unwrap_or_default()
    }

    #[test]
    fn appends_lines_and_reopens_in_append_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = RotatingWriter::open(tmp.path(), 1024, 3).unwrap();
        w.write_line(b"one").unwrap();
        w.sync().unwrap();
        drop(w);
        let mut w = RotatingWriter::open(tmp.path(), 1024, 3).unwrap();
        w.write_line(b"two").unwrap();
        w.flush().unwrap();
        assert_eq!(read(tmp.path(), FILE_NAME), "one\ntwo\n");
    }

    #[test]
    fn rotates_by_size_and_keeps_max_files() {
        let tmp = tempfile::tempdir().unwrap();
        // 10-byte lines ("lineNNNNN\n"), 25-byte limit: two lines per file.
        let mut w = RotatingWriter::open(tmp.path(), 25, 3).unwrap();
        for i in 0..9 {
            w.write_line(format!("line{i:05}").as_bytes()).unwrap();
        }
        w.flush().unwrap();
        let mut names: Vec<String> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [FILE_NAME, "telemetry.ndjson.1", "telemetry.ndjson.2"]
        );
        assert_eq!(read(tmp.path(), FILE_NAME), "line00008\n");
        assert_eq!(
            read(tmp.path(), "telemetry.ndjson.1"),
            "line00006\nline00007\n"
        );
        assert_eq!(
            read(tmp.path(), "telemetry.ndjson.2"),
            "line00004\nline00005\n"
        );
        for name in &names {
            assert!(fs::metadata(tmp.path().join(name)).unwrap().len() <= 25);
        }
    }

    #[test]
    fn single_file_mode_truncates() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = RotatingWriter::open(tmp.path(), 25, 1).unwrap();
        for i in 0..5 {
            w.write_line(format!("line{i:05}").as_bytes()).unwrap();
        }
        w.flush().unwrap();
        assert_eq!(read(tmp.path(), FILE_NAME), "line00004\n");
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    }

    #[test]
    fn oversized_line_goes_into_its_own_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = RotatingWriter::open(tmp.path(), 10, 2).unwrap();
        w.write_line(&[b'x'; 30]).unwrap();
        w.write_line(b"y").unwrap();
        w.flush().unwrap();
        assert_eq!(read(tmp.path(), FILE_NAME), "y\n");
        assert_eq!(read(tmp.path(), "telemetry.ndjson.1").len(), 31);
    }
}
