//! The open-files limit of this process, raised at start.
//!
//! Linux starts a process with a soft limit of 1024 open files (systemd, Docker and a login
//! shell all hand out 1024, with a hard limit of 524288 or more behind it). That number dates
//! from `select()` and is far too low for this server: every database connection, scanned
//! file, archive, CHD and HTTP request is a descriptor, and several jobs run at once. A hash
//! match over a large folder failed in DAT resolve with "Too many open files (os error 24)"
//! while two others were hashing beside it -- the pool could not open its next socket.
//!
//! So the soft limit goes up to the hard one at start, as Go's runtime does for every
//! program. Capped at [`WANTED`]: plenty for any number of jobs, and a child that walks every
//! descriptor up to its limit (mariadbd inherits it) does not pay for a million of them.

/// The soft limit asked for, when the hard limit allows it.
pub const WANTED: u64 = 65536;

/// Raises the soft open-files limit towards [`WANTED`] (never lowers it). Returns the limit
/// before and after, or `None` where there is no such limit (Windows) or it can't be read.
/// Call it once, first thing in `main`, before any thread or child process starts.
#[cfg(unix)]
pub fn raise() -> Option<(u64, u64)> {
    let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: getrlimit only writes the struct it is given.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return None;
    }
    let before = lim.rlim_cur as u64;
    let target = target(before, lim.rlim_max as u64);
    if target <= before {
        return Some((before, before));
    }
    lim.rlim_cur = target as libc::rlim_t;
    // SAFETY: as above; a soft limit at or under the hard one needs no privilege.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) } == 0 {
        return Some((before, target));
    }
    // macOS reports an unlimited hard limit but refuses anything above OPEN_MAX (10240).
    #[cfg(target_os = "macos")]
    {
        lim.rlim_cur = (10240u64.max(before)) as libc::rlim_t;
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) } == 0 {
            return Some((before, lim.rlim_cur as u64));
        }
    }
    Some((before, before))
}

#[cfg(not(unix))]
pub fn raise() -> Option<(u64, u64)> {
    None
}

/// The soft limit to ask for: [`WANTED`] where the hard limit allows, never below `soft`.
fn target(soft: u64, hard: u64) -> u64 {
    WANTED.min(hard).max(soft)
}

/// [`raise`], then a log line saying what it did: info when raised, a warning when the limit
/// stays low (a hard limit under [`WANTED`] that only the service's configuration can lift).
pub fn raise_and_log() {
    match raise() {
        Some((before, after)) if after > before => log::info!("open files limit raised from {before} to {after}"),
        Some((_, after)) if after < 4096 => log::warn!(
            "open files limit is {after}; several jobs at once can run out (\"Too many open files\"). Raise it with LimitNOFILE= (systemd), --ulimit nofile= (Docker) or ulimit -n"
        ),
        _ => {}
    }
}

/// What this process has open right now, by kind, for a "Too many open files" error: the
/// total against the limit, sockets, pipes, and the folders holding the most open files.
/// Linux only (it reads `/proc/self/fd`); `None` elsewhere or when even that can't be opened.
pub fn open_files_summary() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let mut total = 0usize;
        let (mut sockets, mut pipes, mut other) = (0usize, 0usize, 0usize);
        let mut dirs: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for entry in std::fs::read_dir("/proc/self/fd").ok()?.flatten() {
            total += 1;
            let Ok(target) = std::fs::read_link(entry.path()) else { continue };
            let target = target.to_string_lossy().into_owned();
            if target.starts_with("socket:") {
                sockets += 1;
            } else if target.starts_with("pipe:") {
                pipes += 1;
            } else if target.starts_with('/') {
                let dir = std::path::Path::new(&target).parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
                *dirs.entry(dir).or_default() += 1;
            } else {
                other += 1;
            }
        }
        let mut dirs: Vec<_> = dirs.into_iter().collect();
        dirs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let files: usize = dirs.iter().map(|d| d.1).sum();
        let top = dirs.iter().take(3).map(|(d, n)| format!("{n} in {d}")).collect::<Vec<_>>().join(", ");
        let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        // SAFETY: getrlimit only writes the struct it is given.
        let limit = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } == 0 { lim.rlim_cur.to_string() } else { "?".to_string() };
        Some(format!("{total} open of {limit}: {sockets} sockets, {pipes} pipes, {files} files{}{}", if top.is_empty() { String::new() } else { format!(" ({top})") }, if other > 0 { format!(", {other} other") } else { String::new() }))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Whether `message` is a "Too many open files" error (EMFILE, os error 24).
pub fn is_out_of_files(message: &str) -> bool {
    message.contains("os error 24")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goes_up_to_the_hard_limit_but_not_past_wanted() {
        assert_eq!(target(1024, 524288), WANTED);
        assert_eq!(target(1024, 4096), 4096);
        assert_eq!(target(1024, u64::MAX), WANTED);
    }

    #[test]
    fn never_lowers_a_limit_already_higher() {
        assert_eq!(target(1_048_576, 1_048_576), 1_048_576);
        assert_eq!(target(1024, 1024), 1024);
    }

    #[test]
    fn knows_the_out_of_files_error() {
        assert!(is_out_of_files("mariadb server error: io error: Too many open files (os error 24)"));
        assert!(!is_out_of_files("io error: No such file or directory (os error 2)"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn summary_counts_what_is_open() {
        let _held = std::fs::File::open("/proc/self/status").unwrap();
        let summary = open_files_summary().unwrap();
        assert!(summary.contains(" open of "), "{summary}");
    }

    #[cfg(unix)]
    #[test]
    fn raise_leaves_the_soft_limit_at_least_where_it_was() {
        let (before, after) = raise().expect("getrlimit works on unix");
        assert!(after >= before);
    }
}
