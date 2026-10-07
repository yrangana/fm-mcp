//! File helpers for `install`: backups, writes that skip unchanged content,
//! and the marked block in a Codex AGENTS file.

use std::{
    fs,
    io::{self, Write},
    ops::Range,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::guidance::{BEGIN_MARKER, END_MARKER};

/// What happened to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Created,
    Updated,
    Unchanged,
    Removed,
}

impl Action {
    pub fn label(self, dry_run: bool) -> &'static str {
        match (self, dry_run) {
            (Self::Created, false) => "created",
            (Self::Updated, false) => "updated",
            (Self::Removed, false) => "removed",
            (Self::Created, true) => "would create",
            (Self::Updated, true) => "would update",
            (Self::Removed, true) => "would remove",
            (Self::Unchanged, _) => "unchanged",
        }
    }
}

/// One line of the report: a file, what happened to it, and why.
#[derive(Debug, Clone)]
pub struct Change {
    pub path: PathBuf,
    pub action: Action,
    pub what: String,
    pub backup: Option<PathBuf>,
}

/// The outcome of [`write`].
pub struct Written {
    pub action: Action,
    pub backup: Option<PathBuf>,
    /// Directories that had to be created, outermost first.
    pub created_dirs: Vec<PathBuf>,
}

/// Reads a file, or `None` if it doesn't exist.
pub fn read(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Writes `content` to `path` unless it already holds exactly that. An existing
/// file is backed up first and keeps its permissions. With `dry_run`, only
/// reports what would happen.
pub fn write(path: &Path, content: &str, dry_run: bool) -> io::Result<Written> {
    let old = read(path)?;
    if old.as_deref() == Some(content) {
        return Ok(Written {
            action: Action::Unchanged,
            backup: None,
            created_dirs: Vec::new(),
        });
    }
    let action = if old.is_some() {
        Action::Updated
    } else {
        Action::Created
    };
    if dry_run {
        return Ok(Written {
            action,
            backup: None,
            created_dirs: Vec::new(),
        });
    }
    let backup = if old.is_some() {
        Some(backup(path)?)
    } else {
        None
    };
    let parent = path.parent().unwrap_or(Path::new("/"));
    let created_dirs = create_dirs(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(content.as_bytes())?;
    // A new file gets the usual 0644 (a temporary file starts at 0600).
    let permissions = if old.is_some() {
        fs::metadata(path)?.permissions()
    } else {
        fs::Permissions::from_mode(0o644)
    };
    temp.as_file().set_permissions(permissions)?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(Written {
        action,
        backup,
        created_dirs,
    })
}

/// Deletes a file, backing it up first unless it holds only what fm-mcp put
/// there (`ours`). Returns the backup's path.
pub fn remove(path: &Path, ours: bool, dry_run: bool) -> io::Result<Option<PathBuf>> {
    if dry_run {
        return Ok(None);
    }
    let backup = if ours { None } else { Some(backup(path)?) };
    fs::remove_file(path)?;
    Ok(backup)
}

/// Removes directories fm-mcp created, deepest first, but only while empty.
pub fn remove_empty_dirs(dirs: &[PathBuf], dry_run: bool) {
    if dry_run {
        return;
    }
    for dir in dirs.iter().rev() {
        // Fails harmlessly if the user has put something there since.
        let _ = fs::remove_dir(dir);
    }
}

/// Copies `path` to `<path>.fm-mcp-backup-<UTC time>`.
pub fn backup(path: &Path) -> io::Result<PathBuf> {
    let stamp = timestamp();
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".fm-mcp-backup-{stamp}"));
    let mut target = PathBuf::from(&name);
    let mut n = 2;
    while target.exists() {
        let mut numbered = name.clone();
        numbered.push(format!("-{n}"));
        target = PathBuf::from(numbered);
        n += 1;
    }
    fs::copy(path, &target)?;
    Ok(target)
}

/// Creates `dir` and any missing parents; returns those it created, outermost first.
fn create_dirs(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut missing: Vec<PathBuf> = dir
        .ancestors()
        .take_while(|d| !d.exists())
        .map(Path::to_path_buf)
        .collect();
    missing.reverse();
    fs::create_dir_all(dir)?;
    Ok(missing)
}

/// The current UTC time as `YYYYMMDD-HHMMSSZ`.
fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Where the fm-mcp block sits in `content`, including its final newline.
pub fn find_block(content: &str) -> Option<Range<usize>> {
    let start = content
        .match_indices(BEGIN_MARKER)
        .map(|(i, _)| i)
        .find(|&i| i == 0 || content[..i].ends_with('\n'))?;
    let end = start + content[start..].find(END_MARKER)? + END_MARKER.len();
    let end = if content[end..].starts_with('\n') {
        end + 1
    } else {
        end
    };
    Some(start..end)
}

/// Puts `block` into `content`: replaces an existing fm-mcp block, or appends
/// it after a blank line. Returns the new text and the separator added before
/// the block, which [`remove_block`] takes away again.
pub fn insert_block(content: &str, block: &str) -> (String, String) {
    if let Some(range) = find_block(content) {
        let mut out = content.to_owned();
        out.replace_range(range, block);
        return (out, String::new());
    }
    let separator = if content.is_empty() || content.ends_with("\n\n") {
        ""
    } else if content.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    (format!("{content}{separator}{block}"), separator.to_owned())
}

/// Takes the fm-mcp block out of `content`, with the separator that
/// [`insert_block`] added if it is still there. `None` if there is no block.
pub fn remove_block(content: &str, separator: &str) -> Option<String> {
    let range = find_block(content)?;
    let mut before = &content[..range.start];
    let after = &content[range.end..];
    if after.is_empty() && !separator.is_empty() {
        before = before.strip_suffix(separator).unwrap_or(before);
    }
    Some(format!("{before}{after}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: &str = "<!-- fm-mcp:begin x -->\nrules\n<!-- fm-mcp:end -->\n";

    #[test]
    fn insert_then_remove_should_restore_the_original_text() {
        for original in ["", "# Mine\n", "# Mine", "# Mine\n\n"] {
            let (with_block, separator) = insert_block(original, BLOCK);
            assert_eq!(
                remove_block(&with_block, &separator).as_deref(),
                Some(original)
            );
        }
    }

    #[test]
    fn insert_should_leave_a_blank_line_before_the_block() {
        assert_eq!(
            insert_block("# Mine\n", BLOCK).0,
            format!("# Mine\n\n{BLOCK}")
        );
    }

    #[test]
    fn insert_should_replace_an_existing_block_in_place() {
        let old = "a\n<!-- fm-mcp:begin -->\nold\n<!-- fm-mcp:end -->\nb\n";
        assert_eq!(insert_block(old, BLOCK).0, format!("a\n{BLOCK}b\n"));
    }

    #[test]
    fn find_block_should_ignore_a_marker_inside_a_line() {
        assert_eq!(
            find_block("see `<!-- fm-mcp:begin` and <!-- fm-mcp:end -->"),
            None
        );
    }

    #[test]
    fn civil_from_days_should_match_known_dates() {
        assert_eq!(
            (civil_from_days(0), civil_from_days(20_734)),
            ((1970, 1, 1), (2026, 10, 8))
        );
    }

    #[test]
    fn write_should_back_up_and_skip_unchanged_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/file.txt");
        let first = write(&path, "one", false).unwrap();
        let again = write(&path, "one", false).unwrap();
        let changed = write(&path, "two", false).unwrap();
        assert_eq!(
            (
                first.action,
                first.created_dirs.len(),
                again.action,
                changed.action,
                fs::read_to_string(changed.backup.unwrap()).unwrap()
            ),
            (
                Action::Created,
                2,
                Action::Unchanged,
                Action::Updated,
                "one".into()
            )
        );
    }
}
