//! Retention: 7 days of logs, a 1 GiB cap with the oldest day deleted
//! first, never today's file. Finished days are compressed to `.jsonl.gz`.
//! Only files named `es-YYYY-MM-DD.jsonl[.gz]` in the log folder are ever
//! touched, and symlinks are never followed.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use flate2::write::GzEncoder;
use flate2::Compression;

use crate::log::{self, LogFile};

/// Days of logs kept.
pub const KEEP_DAYS: i64 = 7;
/// The size cap over all log files.
pub const CAP_BYTES: u64 = 1 << 30;

/// One file the plan looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileInfo {
    pub day: i64,
    pub bytes: u64,
    pub compressed: bool,
}

/// A file to delete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delete {
    pub day: i64,
    pub compressed: bool,
}

/// Which files to delete: days older than `keep_days` before `today`, then
/// the oldest remaining until the total is at most `cap_bytes`. Today's
/// files are never deleted.
pub fn plan(files: &[FileInfo], today: i64, keep_days: i64, cap_bytes: u64) -> Vec<Delete> {
    let mut sorted: Vec<FileInfo> = files.to_vec();
    sorted.sort_by_key(|f| (f.day, f.compressed));
    let oldest_kept = today.saturating_sub(keep_days.saturating_sub(1));
    let mut deletes = Vec::new();
    let mut kept = Vec::new();
    for f in sorted {
        if f.day < oldest_kept && f.day != today {
            deletes.push(Delete {
                day: f.day,
                compressed: f.compressed,
            });
        } else {
            kept.push(f);
        }
    }
    let mut total: u64 = kept.iter().fold(0u64, |acc, f| acc.saturating_add(f.bytes));
    for f in kept {
        if total <= cap_bytes || f.day >= today {
            break;
        }
        total = total.saturating_sub(f.bytes);
        deletes.push(Delete {
            day: f.day,
            compressed: f.compressed,
        });
    }
    deletes
}

fn path_for(dir: &Path, day: i64, compressed: bool) -> PathBuf {
    let name = log::log_name(day);
    if compressed {
        dir.join(format!("{name}.gz"))
    } else {
        dir.join(name)
    }
}

/// Deletes one log file, only if it is a regular file (never a symlink).
fn delete_file(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    fs::remove_file(path)
}

/// Runs a retention pass. `protect` is a further day never deleted (the
/// day the writer has open, if it differs from today). Returns the bytes
/// left in the folder.
pub fn apply(
    dir: &Path,
    today: i64,
    protect: Option<i64>,
    keep_days: i64,
    cap_bytes: u64,
) -> io::Result<u64> {
    let files: Vec<LogFile> = log::list_logs(dir)?;
    let infos: Vec<FileInfo> = files
        .iter()
        .filter(|f| Some(f.day) != protect)
        .map(|f| FileInfo {
            day: f.day,
            bytes: f.bytes,
            compressed: f.compressed,
        })
        .collect();
    let deletes = plan(&infos, today, keep_days, cap_bytes);
    let mut left: u64 = 0;
    for f in &files {
        let doomed = Some(f.day) != protect
            && deletes
                .iter()
                .any(|d| d.day == f.day && d.compressed == f.compressed);
        if doomed {
            // A failure to delete one file must not stop the pass.
            let _ = delete_file(&f.path);
        } else {
            left = left.saturating_add(f.bytes);
        }
    }
    Ok(left)
}

/// Compresses a finished day's plain file to `.jsonl.gz`: write a temp
/// file, sync, rename into place, then delete the plain file. A day that
/// is already compressed, or has no plain file, is left alone.
pub fn compress(dir: &Path, day: i64) -> io::Result<bool> {
    let plain = path_for(dir, day, false);
    let gz = path_for(dir, day, true);
    let meta = match fs::symlink_metadata(&plain) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if !meta.file_type().is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    if fs::symlink_metadata(&gz).is_ok() {
        // A previous run finished the rename but not the delete.
        delete_file(&plain)?;
        return Ok(true);
    }
    let tmp = dir.join(format!("{}.gz.tmp", log::log_name(day)));
    if let Ok(m) = fs::symlink_metadata(&tmp) {
        if !m.file_type().is_file() {
            return Err(io::Error::other("temp path is not a regular file"));
        }
        fs::remove_file(&tmp)?;
    }
    let result = (|| -> io::Result<()> {
        let out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        out.set_permissions(fs::Permissions::from_mode(0o600))?;
        let mut encoder = GzEncoder::new(out, Compression::default());
        let mut input = File::open(&plain)?;
        io::copy(&mut input, &mut encoder)?;
        let mut out = encoder.finish()?;
        out.flush()?;
        out.sync_all()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    fs::rename(&tmp, &gz)?;
    delete_file(&plain)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::tests::{event, temp_dir};
    use crate::log::LogWriter;
    use crate::timefmt::DAY_MS;

    fn f(day: i64, bytes: u64, compressed: bool) -> FileInfo {
        FileInfo {
            day,
            bytes,
            compressed,
        }
    }

    fn days(d: &[Delete]) -> Vec<i64> {
        d.iter().map(|x| x.day).collect()
    }

    #[test]
    fn deletes_old_days() {
        let files: Vec<FileInfo> = (90..=100).map(|d| f(d, 10, d != 100)).collect();
        assert_eq!(days(&plan(&files, 100, 7, 1 << 30)), vec![90, 91, 92, 93]);
    }

    #[test]
    fn cap_deletes_oldest_first_never_today() {
        let files = vec![f(98, 50, true), f(99, 50, true), f(100, 500, false)];
        assert_eq!(days(&plan(&files, 100, 7, 560)), vec![98]);
        assert_eq!(days(&plan(&files, 100, 7, 10)), vec![98, 99]);
        // Today alone over the cap is kept.
        assert!(plan(&[f(100, 1000, false)], 100, 7, 10).is_empty());
        // A future-dated file (clock moved back) is not deleted by the cap.
        assert!(plan(&[f(101, 1000, false)], 100, 7, 10).is_empty());
        assert!(plan(&[], 100, 7, 10).is_empty());
    }

    #[test]
    fn compress_and_apply() {
        let dir = temp_dir("ret");
        let mut w = LogWriter::open(&dir).unwrap();
        let base = 20_000;
        for d in 0..10 {
            w.append(event((base + d) * DAY_MS + 1, "c")).unwrap();
        }
        let finished = w.take_finished();
        assert_eq!(finished.len(), 9);
        drop(w);
        for day in finished {
            assert!(compress(&dir, day).unwrap());
        }
        assert!(!compress(&dir, base).unwrap());
        // Compressed days still read and link.
        let all = log::read_events(&dir, 0, i64::MAX / 2, "", 100).unwrap();
        assert_eq!(all.events.len(), 10);
        assert!(all.chain_ok);
        let mode = fs::metadata(path_for(&dir, base, true))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        // Foreign files and symlinks named like logs are not touched.
        fs::write(dir.join("notes.txt"), "keep").unwrap();
        let outside = temp_dir("ret-outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("victim"), "x").unwrap();
        std::os::unix::fs::symlink(outside.join("victim"), dir.join("es-2000-01-01.jsonl"))
            .unwrap();

        assert!(apply(&dir, base + 9, Some(base), 7, 1 << 30).unwrap() > 0);
        assert!(path_for(&dir, base, true).exists());
        let left = apply(&dir, base + 9, None, 7, 1 << 30).unwrap();
        let remaining = log::list_logs(&dir).unwrap();
        assert_eq!(remaining.len(), 7);
        assert_eq!(remaining[0].day, base + 3);
        assert_eq!(left, remaining.iter().map(|f| f.bytes).sum::<u64>());
        assert!(dir.join("notes.txt").exists());
        assert!(outside.join("victim").exists());
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn compress_finishes_an_interrupted_run() {
        let dir = temp_dir("ret2");
        let mut w = LogWriter::open(&dir).unwrap();
        w.append(event(20_000 * DAY_MS, "c")).unwrap();
        drop(w);
        let plain = path_for(&dir, 20_000, false);
        let copy = dir.join("copy");
        fs::copy(&plain, &copy).unwrap();
        assert!(compress(&dir, 20_000).unwrap());
        fs::rename(&copy, &plain).unwrap();
        assert!(compress(&dir, 20_000).unwrap());
        assert!(!plain.exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
