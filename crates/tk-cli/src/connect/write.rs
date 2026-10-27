//! The write path: read a harness config safely, ask on the terminal, check
//! that nothing changed since the diff was shown (case 55), back up, and
//! replace the file atomically.
//!
//! What it never does: follow a symlink to write (the link's target could be
//! anywhere, and a rename would replace the link with a file); write into a
//! managed file (case 65); take "yes" from standard input, where a script
//! could pipe it by accident (the answer comes from `/dev/tty` only).
//!
//! What the question does not prove (Mac test w1, M4): that a person
//! answered. Any process that can start a pty has a `/dev/tty`; the box test
//! answered it through `script -qec`, as an agent with a shell could. The
//! question guards against a mistake, not against an agent: an agent that
//! runs as you can write the same file without tk. What stands against an
//! agent is the gate (each harness's config file is a control path, so a
//! call naming it asks) and the boundary (srt and sbx keep the file out of
//! reach); under `--boundary none` with `bash: allow` it has your rights.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::spec::sha256_hex;

/// Config files bigger than this are not configs tk should be rewriting.
pub const MAX_CONFIG_BYTES: u64 = 32 * 1024 * 1024;

/// The file as read for the dry run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current {
    /// `None` when the file does not exist.
    pub bytes: Option<Vec<u8>>,
    /// SHA-256 of the bytes, or of nothing (`e3b0c4...`) when absent.
    pub sha256: String,
    /// Permission bits to keep on the new file.
    pub mode: u32,
}

impl Current {
    pub fn text(&self) -> Result<&str, String> {
        match &self.bytes {
            None => Ok(""),
            Some(b) => std::str::from_utf8(b).map_err(|_| "the file is not UTF-8".to_string()),
        }
    }
}

/// Reads `path` without following a symlink at the last component.
pub fn read_current(path: &Path) -> Result<Current, String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Current {
            bytes: None,
            sha256: sha256_hex(b""),
            mode: 0o600,
        }),
        Err(e) => Err(format!("{}: {e}", path.display())),
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(format!(
                    "{} is a symlink; tk does not write through links (edit the target yourself, or replace the link with a file)",
                    path.display()
                ));
            }
            if !meta.is_file() {
                return Err(format!("{} is not a regular file", path.display()));
            }
            if meta.len() > MAX_CONFIG_BYTES {
                return Err(format!(
                    "{} is larger than {MAX_CONFIG_BYTES} bytes",
                    path.display()
                ));
            }
            let mut bytes = Vec::with_capacity(meta.len() as usize);
            File::open(path)
                .and_then(|f| f.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes))
                .map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(Current {
                sha256: sha256_hex(&bytes),
                bytes: Some(bytes),
                mode: meta.mode() & 0o777,
            })
        }
    }
}

/// Asks a yes-or-no question. The real one talks to `/dev/tty`.
pub trait Confirm {
    fn confirm(&mut self, question: &str) -> Result<bool, String>;
}

/// The terminal. Fails when there is none (a cron job, an agent's shell
/// without a terminal): `--write` then refuses.
pub struct Tty;

impl Confirm for Tty {
    fn confirm(&mut self, question: &str) -> Result<bool, String> {
        let mut tty = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .map_err(|e| format!("no terminal to confirm on (/dev/tty: {e}); nothing written"))?;
        write!(tty, "{question} [y/N] ").map_err(|e| e.to_string())?;
        tty.flush().map_err(|e| e.to_string())?;
        let mut line = String::new();
        BufReader::new(&tty)
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        let a = line.trim();
        Ok(a.eq_ignore_ascii_case("y") || a.eq_ignore_ascii_case("yes"))
    }
}

/// What a write did, for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub sha256_after: String,
    pub backup: Option<PathBuf>,
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn make_private_dir(dir: &Path) -> Result<(), String> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))
}

/// Copies the old bytes to `backups/<harness>-<ms>-<file name>`, mode 0600.
fn backup(backups: &Path, harness: &str, path: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    make_private_dir(backups)?;
    let base = path
        .file_name()
        .map_or("config".into(), |n| n.to_string_lossy().into_owned());
    let dest = backups.join(format!("{harness}-{}-{base}", now_ms()));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&dest)
        .map_err(|e| format!("backup {}: {e}", dest.display()))?;
    f.write_all(bytes)
        .and_then(|_| f.sync_all())
        .map_err(|e| format!("backup {}: {e}", dest.display()))?;
    Ok(dest)
}

/// Writes `new_text` to `path` after a terminal confirmation, if the file
/// still hashes to `expected_sha256`. The old file is backed up first.
pub fn apply(
    path: &Path,
    harness: &str,
    expected_sha256: &str,
    new_text: &str,
    backups: &Path,
    confirm: &mut dyn Confirm,
) -> Result<Written, String> {
    if !confirm.confirm(&format!("Write this change to {}?", path.display()))? {
        return Err("not confirmed; nothing written".into());
    }
    // Re-read after the question: the user may have taken minutes, and an
    // agent or the harness itself may have written in between (case 55).
    let now = read_current(path)?;
    if now.sha256 != expected_sha256 {
        return Err(format!(
            "{} changed since the diff was shown (sha256 {} now, {} then); nothing written. Run the dry run again.",
            path.display(),
            now.sha256,
            expected_sha256
        ));
    }
    let backup_path = match &now.bytes {
        Some(b) => Some(backup(backups, harness, path, b)?),
        None => None,
    };
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent folder", path.display()))?;
    make_private_dir(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("{} has no file name", path.display()))?
        .to_string_lossy()
        .into_owned();
    let tmp = parent.join(format!(".{name}.tk-{}.tmp", std::process::id()));
    let result = (|| -> Result<(), String> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(now.mode)
            .open(&tmp)
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
        // `mode` is filtered by the umask; set it exactly.
        f.set_permissions(fs::Permissions::from_mode(now.mode))
            .map_err(|e| e.to_string())?;
        f.write_all(new_text.as_bytes())
            .and_then(|_| f.sync_all())
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
        fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Ok(d) = File::open(parent) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(Written {
        sha256_after: sha256_hex(new_text.as_bytes()),
        backup: backup_path,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub struct Answer(pub bool, pub Option<Box<dyn FnMut()>>);

    impl Confirm for Answer {
        fn confirm(&mut self, _q: &str) -> Result<bool, String> {
            if let Some(f) = self.1.as_mut() {
                f();
            }
            Ok(self.0)
        }
    }

    pub fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "tk-connect-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn absent_file_reads_as_empty() {
        let d = scratch("absent");
        let c = read_current(&d.join("nope.json")).unwrap();
        assert_eq!(c.bytes, None);
        assert_eq!(c.sha256, sha256_hex(b""));
        assert_eq!(c.text().unwrap(), "");
    }

    #[test]
    fn writes_after_yes_keeps_the_mode_and_backs_up() {
        let d = scratch("write");
        let p = d.join("cfg.json");
        fs::write(&p, "{}").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
        let c = read_current(&p).unwrap();
        let w = apply(
            &p,
            "pi",
            &c.sha256,
            "{\"a\": 1}",
            &d.join("backups"),
            &mut Answer(true, None),
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "{\"a\": 1}");
        assert_eq!(fs::metadata(&p).unwrap().mode() & 0o777, 0o640);
        let b = w.backup.unwrap();
        assert_eq!(fs::read_to_string(&b).unwrap(), "{}");
        assert_eq!(fs::metadata(&b).unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            fs::metadata(d.join("backups")).unwrap().mode() & 0o777,
            0o700
        );
        // No temp file left.
        assert_eq!(fs::read_dir(&d).unwrap().count(), 2);
    }

    #[test]
    fn new_file_and_folders_are_private() {
        let d = scratch("new");
        let p = d.join("a/b/mcp.json");
        let c = read_current(&p).unwrap();
        apply(
            &p,
            "pi",
            &c.sha256,
            "{}\n",
            &d.join("bk"),
            &mut Answer(true, None),
        )
        .unwrap();
        assert_eq!(fs::metadata(&p).unwrap().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(d.join("a/b")).unwrap().mode() & 0o777, 0o700);
        assert!(!d.join("bk").exists(), "nothing to back up");
    }

    #[test]
    fn no_means_nothing_written() {
        let d = scratch("no");
        let p = d.join("c.json");
        fs::write(&p, "{}").unwrap();
        let c = read_current(&p).unwrap();
        let e = apply(
            &p,
            "pi",
            &c.sha256,
            "x",
            &d.join("bk"),
            &mut Answer(false, None),
        )
        .unwrap_err();
        assert!(e.contains("not confirmed"));
        assert_eq!(fs::read_to_string(&p).unwrap(), "{}");
    }

    #[test]
    fn a_change_while_the_question_was_open_is_refused() {
        // Case 55.
        let d = scratch("race");
        let p = d.join("c.json");
        fs::write(&p, "{}").unwrap();
        let c = read_current(&p).unwrap();
        let p2 = p.clone();
        let mut ans = Answer(
            true,
            Some(Box::new(move || fs::write(&p2, "{\"other\": 1}").unwrap())),
        );
        let e = apply(
            &p,
            "pi",
            &c.sha256,
            "{\"mine\": 1}",
            &d.join("bk"),
            &mut ans,
        )
        .unwrap_err();
        assert!(e.contains("changed since the diff was shown"), "{e}");
        assert_eq!(fs::read_to_string(&p).unwrap(), "{\"other\": 1}");
    }

    #[test]
    fn symlinks_and_folders_are_refused() {
        let d = scratch("link");
        let target = d.join("real.json");
        fs::write(&target, "{}").unwrap();
        let link = d.join("link.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(read_current(&link).unwrap_err().contains("symlink"));
        assert!(read_current(&d).unwrap_err().contains("not a regular file"));
    }
}
