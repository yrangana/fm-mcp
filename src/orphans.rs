//! Protection against orphaned `fm serve` processes (plan R13, decision D7).
//!
//! If a client force-kills fm-mcp, fm-mcp can't stop its `fm serve` child, and
//! macOS has no "stop my child when I die" setting. Two defences:
//! - a watchdog process per child, which stops `fm serve` once fm-mcp is gone;
//! - a clean-up when fm-mcp starts, which stops orphans left by earlier sessions.

use std::{
    os::unix::{fs::MetadataExt, net::UnixStream},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use nix::{
    sys::signal::{Signal, kill},
    unistd::{Pid, getppid, getuid},
};
use tracing::{info, warn};

pub const SOCKET_DIR_PREFIX: &str = "fm-mcp-";
pub const SOCKET_NAME: &str = "fm.sock";
const WATCH_POLL: Duration = Duration::from_millis(500);
const STOP_GRACE: Duration = Duration::from_secs(2);
/// A socket folder younger than this may belong to a session that is still starting.
const STALE_DIR_AGE: Duration = Duration::from_secs(60);

/// Runs the watchdog until `child` exits, or until `parent` (fm-mcp) disappears,
/// in which case it stops `child` and removes `socket_dir` first.
pub fn watch(parent: i32, child: i32, socket_dir: &Path) {
    let parent = Pid::from_raw(parent);
    let child = Pid::from_raw(child);
    loop {
        thread::sleep(WATCH_POLL);
        if kill(child, None).is_err() {
            return;
        }
        // Once fm-mcp dies, this process is re-parented to launchd.
        if getppid() != parent {
            eprintln!(
                "fm-mcp watchdog: fm-mcp exited without cleaning up; stopping fm serve pid={child}"
            );
            stop(child);
            remove_socket_dir(socket_dir);
            return;
        }
    }
}

/// An `fm serve` started by fm-mcp that no longer has an fm-mcp parent.
#[derive(Debug, PartialEq, Eq)]
pub struct Orphan {
    pub pid: i32,
    pub socket: PathBuf,
}

/// The current user's orphaned `fm serve` processes: started on an fm-mcp
/// socket path, now owned by launchd (parent pid 1).
pub fn find_orphans() -> Vec<Orphan> {
    match Command::new("/bin/ps")
        // `-ww`: never truncate the command line, or the socket path could be cut.
        .args(["-A", "-ww", "-o", "pid=,ppid=,uid=,command="])
        .output()
    {
        Ok(output) => parse_ps(&String::from_utf8_lossy(&output.stdout), getuid().as_raw()),
        Err(e) => {
            warn!("could not list processes to find orphans: {e}");
            Vec::new()
        }
    }
}

/// Stops orphaned `fm serve` processes and removes stale socket folders under
/// `bases`. Safe to run while other fm-mcp sessions are running: their children
/// have a live parent, and their folders are young or have a listening socket.
pub fn clean_up(bases: &[PathBuf]) {
    for orphan in find_orphans() {
        info!(
            "stopping orphaned fm serve pid={} socket={}",
            orphan.pid,
            orphan.socket.display()
        );
        stop(Pid::from_raw(orphan.pid));
        if let Some(dir) = orphan.socket.parent() {
            remove_socket_dir(dir);
        }
    }
    for base in bases {
        remove_stale_socket_dirs(base);
    }
}

fn parse_ps(text: &str, uid: u32) -> Vec<Orphan> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid: i32 = fields.next()?.parse().ok()?;
            let ppid: i32 = fields.next()?.parse().ok()?;
            let owner: u32 = fields.next()?.parse().ok()?;
            if ppid != 1 || owner != uid {
                return None;
            }
            // Expect "... serve --socket <dir>/fm-mcp-XXXX/fm.sock".
            let words: Vec<&str> = fields.collect();
            let flag = words.iter().position(|w| *w == "--socket")?;
            if flag == 0 || words[flag - 1] != "serve" {
                return None;
            }
            let socket = PathBuf::from(words.get(flag + 1)?);
            is_fm_mcp_socket(&socket).then_some(Orphan { pid, socket })
        })
        .collect()
}

fn is_fm_mcp_socket(socket: &Path) -> bool {
    socket.file_name().is_some_and(|n| n == SOCKET_NAME)
        && socket.parent().is_some_and(is_fm_mcp_dir)
}

fn is_fm_mcp_dir(dir: &Path) -> bool {
    dir.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(SOCKET_DIR_PREFIX))
}

/// SIGTERM, then SIGKILL if `pid` is still alive after the grace period.
fn stop(pid: Pid) {
    if kill(pid, Signal::SIGTERM).is_err() {
        return;
    }
    let polls = STOP_GRACE.as_millis() / 50;
    for _ in 0..polls {
        thread::sleep(Duration::from_millis(50));
        if kill(pid, None).is_err() {
            return;
        }
    }
    let _ = kill(pid, Signal::SIGKILL);
}

fn remove_socket_dir(dir: &Path) {
    if is_fm_mcp_dir(dir) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Removes this user's `fm-mcp-*` folders under `base` that are old and have
/// nothing listening on their socket: left behind when fm-mcp was force-killed.
fn remove_stale_socket_dirs(base: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let uid = getuid().as_raw();
    for entry in entries.flatten() {
        let dir = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&dir) else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > STALE_DIR_AGE);
        if is_fm_mcp_dir(&dir)
            && meta.is_dir()
            && meta.uid() == uid
            && old
            && UnixStream::connect(dir.join(SOCKET_NAME)).is_err()
        {
            info!("removing stale socket folder {}", dir.display());
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PS: &str = "\
  101     1   501 /usr/bin/fm serve --socket /var/folders/x/T/fm-mcp-AbC123/fm.sock
  102   900   501 /usr/bin/fm serve --socket /var/folders/x/T/fm-mcp-DeF456/fm.sock
  103     1   502 /usr/bin/fm serve --socket /tmp/fm-mcp-GhI789/fm.sock
  104     1   501 /usr/bin/fm serve --socket /tmp/my-own.sock
  105     1   501 /usr/bin/fm respond --socket /tmp/fm-mcp-JkL000/fm.sock
  106     1   501 /usr/libexec/something else
";

    #[test]
    fn parse_ps_should_find_only_this_users_fm_mcp_orphans() {
        assert_eq!(
            parse_ps(PS, 501),
            vec![Orphan {
                pid: 101,
                socket: PathBuf::from("/var/folders/x/T/fm-mcp-AbC123/fm.sock"),
            }]
        );
    }

    #[test]
    fn remove_socket_dir_should_refuse_other_folders() {
        let base = tempfile::tempdir().unwrap();
        let other = base.path().join("not-ours");
        std::fs::create_dir(&other).unwrap();
        remove_socket_dir(&other);
        assert!(other.exists());
    }

    #[test]
    fn stale_dir_sweep_should_keep_young_folders() {
        let base = tempfile::tempdir().unwrap();
        let young = base.path().join("fm-mcp-young1");
        std::fs::create_dir(&young).unwrap();
        remove_stale_socket_dirs(base.path());
        assert!(young.exists());
    }
}
