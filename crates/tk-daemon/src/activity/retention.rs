//! B7: how long the app's activity log is kept.
//!
//! Your call of 16 Oct (plan, section 3, point 5): 7 days of records, 3 days
//! of screenshots, a 1 GB cap with the oldest day deleted first, and
//! screenshots stopped under a free-disk floor you set. The collector keeps
//! its own log on the same rule (`macos/activity-collector/`).
//!
//! The plan is a pure function over what is on disk. Applying it deletes only
//! names this module wrote, never through a symlink, and never today's day.

use std::fs;
use std::path::{Path, PathBuf};

pub const KEEP_LOG_DAYS: i64 = 7;
pub const KEEP_SCREENSHOT_DAYS: i64 = 3;
pub const CAP_BYTES: u64 = 1024 * 1024 * 1024;
/// The free-disk floor below which screenshots stop, until you set another.
pub const DEFAULT_SCREENSHOT_FLOOR_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// What one item on disk holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    /// A day's log file, `app-YYYY-MM-DD.jsonl`.
    Log,
    /// A day's screenshot folder, `screenshots/YYYY-MM-DD/`.
    Screenshots,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// Days since the Unix epoch, UTC.
    pub day: i64,
    pub bytes: u64,
    pub kind: ItemKind,
    pub path: PathBuf,
}

/// The items to delete, oldest first.
pub fn plan(items: &[Item], today: i64, cap_bytes: u64) -> Vec<Item> {
    let mut keep: Vec<&Item> = Vec::new();
    let mut delete: Vec<Item> = Vec::new();
    for item in items {
        let limit = match item.kind {
            ItemKind::Log => KEEP_LOG_DAYS,
            ItemKind::Screenshots => KEEP_SCREENSHOT_DAYS,
        };
        // Day `today - limit + 1` .. `today` stay: `limit` days in all.
        if item.day != today && today.saturating_sub(item.day) >= limit {
            delete.push(item.clone());
        } else {
            keep.push(item);
        }
    }
    keep.sort_by_key(|item| (item.day, item.kind == ItemKind::Log));
    let mut total: u64 = keep
        .iter()
        .fold(0u64, |sum, item| sum.saturating_add(item.bytes));
    for item in keep {
        if total <= cap_bytes {
            break;
        }
        if item.day == today {
            continue;
        }
        total = total.saturating_sub(item.bytes);
        delete.push(item.clone());
    }
    delete.sort_by_key(|item| item.day);
    delete
}

/// Whether a new screenshot may be written, given the free bytes on the disk
/// that holds the log.
pub fn screenshots_allowed(free_bytes: Option<u64>, floor_bytes: u64) -> bool {
    // Unknown free space: keep taking them, the 1 GB cap still holds.
    free_bytes.is_none_or(|free| free >= floor_bytes)
}

/// `YYYY-MM-DD` for a day number, proleptic Gregorian, UTC.
pub fn day_name(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The day number of `YYYY-MM-DD`, or `None`.
pub fn parse_day(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = &text[range];
        if !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let (y, m, d) = (number(0..4)?, number(5..7)?, number(8..10)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let day = days_from_civil(y, m, d);
    (civil_from_days(day) == (y, m, d)).then_some(day)
}

/// The UTC day of a time in milliseconds since the epoch.
pub fn day_of_ms(ms: i64) -> i64 {
    ms.div_euclid(86_400_000)
}

// Howard Hinnant's algorithms, for dates without a date crate.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub const LOG_PREFIX: &str = "app-";
pub const LOG_SUFFIX: &str = ".jsonl";
pub const SCREENSHOT_DIR: &str = "screenshots";

/// What sits in the log folder, by the names this module writes. Anything
/// else, and any symlink, is left out, so it is never deleted.
pub fn scan(dir: &Path) -> Vec<Item> {
    let mut items = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(day) = name
                .strip_prefix(LOG_PREFIX)
                .and_then(|rest| rest.strip_suffix(LOG_SUFFIX))
                .and_then(parse_day)
            else {
                continue;
            };
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.file_type().is_file() {
                items.push(Item {
                    day,
                    bytes: meta.len(),
                    kind: ItemKind::Log,
                    path: entry.path(),
                });
            }
        }
    }
    let shots = dir.join(SCREENSHOT_DIR);
    let shots_is_dir = fs::symlink_metadata(&shots).is_ok_and(|m| m.file_type().is_dir());
    if shots_is_dir {
        if let Ok(entries) = fs::read_dir(&shots) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let Some(day) = parse_day(&name) else {
                    continue;
                };
                let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                    continue;
                };
                if meta.file_type().is_dir() {
                    items.push(Item {
                        day,
                        bytes: folder_bytes(&entry.path()),
                        kind: ItemKind::Screenshots,
                        path: entry.path(),
                    });
                }
            }
        }
    }
    items
}

/// Bytes of the plain files directly in a screenshot day folder.
fn folder_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Ok(meta) = fs::symlink_metadata(entry.path()) {
                if meta.file_type().is_file() {
                    total = total.saturating_add(meta.len());
                }
            }
        }
    }
    total
}

/// Deletes what the plan names. A screenshot day folder loses its plain
/// files, then the folder if it is empty; a symlink inside is never followed.
/// Returns the count deleted.
pub fn apply(deletes: &[Item]) -> usize {
    let mut count = 0;
    for item in deletes {
        match item.kind {
            ItemKind::Log => {
                let is_file =
                    fs::symlink_metadata(&item.path).is_ok_and(|m| m.file_type().is_file());
                if is_file && fs::remove_file(&item.path).is_ok() {
                    count += 1;
                }
            }
            ItemKind::Screenshots => {
                if let Ok(entries) = fs::read_dir(&item.path) {
                    for entry in entries.flatten() {
                        let is_file = fs::symlink_metadata(entry.path())
                            .is_ok_and(|m| m.file_type().is_file());
                        if is_file {
                            let _ = fs::remove_file(entry.path());
                        }
                    }
                }
                if fs::remove_dir(&item.path).is_ok() {
                    count += 1;
                }
            }
        }
    }
    count
}

/// Free bytes on the disk that holds `path`, where the platform tells.
pub fn free_bytes(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: a valid NUL-terminated path and a zeroed out-parameter.
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if rc != 0 {
            return None;
        }
        #[allow(clippy::unnecessary_cast)]
        (stat.f_bavail as u64).checked_mul(stat.f_frsize as u64)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(day: i64, bytes: u64, kind: ItemKind) -> Item {
        Item {
            day,
            bytes,
            kind,
            path: PathBuf::from(format!("/x/{day}-{kind:?}")),
        }
    }

    #[test]
    fn days_round_trip() {
        for day in [-1, 0, 1, 19_000, 20_377, 30_000] {
            assert_eq!(parse_day(&day_name(day)), Some(day));
        }
        assert_eq!(day_name(0), "1970-01-01");
        assert_eq!(parse_day("2026-10-16"), Some(20_742));
        assert_eq!(parse_day("2026-02-30"), None);
        assert_eq!(parse_day("2026-1-16x"), None);
        assert_eq!(parse_day("+026-10-16"), None);
        assert_eq!(day_of_ms(-1), -1);
    }

    #[test]
    fn keeps_seven_days_of_logs_and_three_of_screenshots() {
        let today = 100;
        let items: Vec<Item> = (90..=100)
            .flat_map(|d| {
                [
                    item(d, 10, ItemKind::Log),
                    item(d, 10, ItemKind::Screenshots),
                ]
            })
            .collect();
        let deleted = plan(&items, today, CAP_BYTES);
        let logs: Vec<i64> = deleted
            .iter()
            .filter(|i| i.kind == ItemKind::Log)
            .map(|i| i.day)
            .collect();
        let shots: Vec<i64> = deleted
            .iter()
            .filter(|i| i.kind == ItemKind::Screenshots)
            .map(|i| i.day)
            .collect();
        assert_eq!(logs, (90..=93).collect::<Vec<_>>());
        assert_eq!(shots, (90..=97).collect::<Vec<_>>());
    }

    #[test]
    fn the_cap_deletes_the_oldest_day_first_and_never_today() {
        let today = 10;
        let items = vec![
            item(8, 600, ItemKind::Log),
            item(9, 600, ItemKind::Log),
            item(10, 2_000, ItemKind::Log),
            item(9, 50, ItemKind::Screenshots),
        ];
        let deleted = plan(&items, today, 1_000);
        let days: Vec<(i64, ItemKind)> = deleted.iter().map(|i| (i.day, i.kind)).collect();
        // Day 8, then day 9's screenshots and log; today stays over the cap.
        assert_eq!(days.len(), 3);
        assert!(days.iter().all(|(d, _)| *d < today));
    }

    #[test]
    fn under_the_cap_nothing_recent_goes() {
        let items = vec![item(99, 5, ItemKind::Log), item(100, 5, ItemKind::Log)];
        assert!(plan(&items, 100, CAP_BYTES).is_empty());
    }

    #[test]
    fn screenshots_stop_under_the_floor() {
        assert!(screenshots_allowed(Some(3), 2));
        assert!(!screenshots_allowed(Some(1), 2));
        assert!(screenshots_allowed(None, 2));
    }

    #[test]
    fn scan_and_apply_touch_only_our_names_and_never_follow_links() {
        let dir = std::env::temp_dir().join(format!("activity-retention-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("screenshots/2026-10-01")).unwrap();
        fs::write(dir.join("app-2026-10-01.jsonl"), b"x").unwrap();
        fs::write(dir.join("notes.txt"), b"keep").unwrap();
        fs::write(dir.join("screenshots/2026-10-01/a.png"), b"png").unwrap();
        let outside = dir.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("precious.txt"), b"keep").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, dir.join("screenshots/2026-10-02")).unwrap();
            std::os::unix::fs::symlink(
                outside.join("precious.txt"),
                dir.join("app-2026-10-03.jsonl"),
            )
            .unwrap();
            std::os::unix::fs::symlink(
                outside.join("precious.txt"),
                dir.join("screenshots/2026-10-01/link.png"),
            )
            .unwrap();
        }
        let items = scan(&dir);
        assert_eq!(items.len(), 2, "{items:?}");
        let today = parse_day("2026-10-16").unwrap();
        let deleted = plan(&items, today, CAP_BYTES);
        assert_eq!(
            apply(&deleted),
            1,
            "the folder keeps its link, so only the log goes"
        );
        assert!(!dir.join("app-2026-10-01.jsonl").exists());
        assert!(!dir.join("screenshots/2026-10-01/a.png").exists());
        assert!(dir.join("notes.txt").exists());
        assert!(outside.join("precious.txt").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn free_bytes_reads_a_real_disk() {
        #[cfg(unix)]
        assert!(free_bytes(&std::env::temp_dir()).is_some());
    }
}
