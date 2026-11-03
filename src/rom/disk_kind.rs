//! Telling solid-state storage from a spinning disk where `sysinfo` can't
//! ([`super::hash::StorageKind::for_path`] asks this only after `sysinfo` said "unknown").
//!
//! `sysinfo` looks up the mount's device under `/sys/block`. A ZFS dataset has no device
//! there -- its "device" is the dataset name (`tank/roms`) -- so every ZFS mount came back
//! unknown, which counts as a hard disk: a pool of NVMe drives got one large file at a time.
//! Here the pool is resolved to the disks that hold its data (`zpool status -P`) and each
//! one's `queue/rotational` is read. Log, cache (L2ARC) and spare devices are left out: they
//! don't hold the files a scan reads.
//!
//! Linux only; elsewhere every answer is `None` and the caller keeps its fallback.

use std::path::{Path, PathBuf};

/// Whether `pool`'s data disks are all solid-state: `Some(true)` all are, `Some(false)` at
/// least one spins, `None` when it can't be told (no `zpool`, a file vdev, a timeout).
pub fn zfs_pool_solid_state(pool: &str) -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        let status = zpool_status(pool)?;
        all_solid_state(zpool_data_devices(&status).iter().map(|d| block_device_solid_state(d)))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pool;
        None
    }
}

/// Whether the block device at `dev` (`/dev/...`, a partition, a by-id link or a
/// device-mapper node) is solid-state, from its disk's `queue/rotational`.
pub fn block_device_solid_state(dev: &Path) -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        let real = dev.canonicalize().ok()?;
        let name = real.file_name()?;
        let sys = Path::new("/sys/class/block").join(name).canonicalize().ok()?;
        // A partition's directory sits inside its disk's, and only the disk has a queue.
        let disk = if sys.join("partition").exists() { sys.parent()?.to_path_buf() } else { sys };
        match std::fs::read_to_string(disk.join("queue/rotational")).ok()?.trim() {
            "0" => Some(true),
            "1" => Some(false),
            _ => None,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = dev;
        None
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// `Some(false)` if any disk spins, `Some(true)` if every disk is known and solid-state,
/// `None` otherwise (none listed, or one couldn't be read).
fn all_solid_state(disks: impl Iterator<Item = Option<bool>>) -> Option<bool> {
    let mut any = false;
    let mut unknown = false;
    for disk in disks {
        any = true;
        match disk {
            Some(false) => return Some(false),
            Some(true) => {}
            None => unknown = true,
        }
    }
    (any && !unknown).then_some(true)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// The device paths in `zpool status -P` output that hold the pool's data: every `/...` row
/// under the pool's own row, minus the `logs`, `cache` and `spares` sections. `special` and
/// `dedup` hold metadata and count. Sections sit at the pool row's indent, devices deeper.
fn zpool_data_devices(status: &str) -> Vec<PathBuf> {
    let mut in_config = false;
    let mut pool_indent = None;
    let mut counts = true;
    let mut out = Vec::new();
    for line in status.lines() {
        let trimmed = line.trim_start();
        if !in_config {
            in_config = trimmed.starts_with("config:");
            continue;
        }
        if trimmed.starts_with("errors:") {
            break;
        }
        let Some(first) = trimmed.split_whitespace().next() else { continue };
        if first == "NAME" {
            continue;
        }
        let indent = line.len() - trimmed.len();
        match pool_indent {
            None => {
                pool_indent = Some(indent);
                continue;
            }
            Some(pool) if indent <= pool => {
                counts = !matches!(first, "logs" | "cache" | "spares");
                continue;
            }
            Some(_) => {}
        }
        if counts && first.starts_with('/') {
            out.push(PathBuf::from(first));
        }
    }
    out
}

/// `zpool status -P <pool>`, or `None` if `zpool` isn't there, fails, or takes over five
/// seconds (a suspended pool can hang it). A systemd service's PATH may lack `/usr/sbin`, so
/// the usual places are tried too.
#[cfg(target_os = "linux")]
fn zpool_status(pool: &str) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    for bin in ["zpool", "/usr/sbin/zpool", "/sbin/zpool", "/usr/local/sbin/zpool"] {
        let Ok(mut child) = Command::new(bin).args(["status", "-P", pool]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else { continue };
        let mut stdout = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut s = String::new();
            stdout.read_to_string(&mut s).ok().map(|_| s)
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let text = reader.join().ok().flatten();
        return if status?.success() { text } else { None };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "  pool: tank
 state: ONLINE
  scan: scrub repaired 0B in 00:10:12 with 0 errors on Sun Nov  1 00:34:13 2026
config:

\tNAME                                STATE     READ WRITE CKSUM
\ttank                                ONLINE       0     0     0
\t  mirror-0                          ONLINE       0     0     0
\t    /dev/nvme0n1p1                  ONLINE       0     0     0
\t    /dev/disk/by-id/nvme-disk-b     ONLINE       0     0     0
\tspecial
\t  /dev/nvme2n1                      ONLINE       0     0     0
\tlogs
\t  /dev/sdc1                         ONLINE       0     0     0
\tcache
\t  /dev/sdd                          ONLINE       0     0     0
\tspares
\t  /dev/sde                          AVAIL

errors: No known data errors
";

    #[test]
    fn only_the_pools_data_and_metadata_disks_count() {
        let got = zpool_data_devices(STATUS);
        assert_eq!(got, ["/dev/nvme0n1p1", "/dev/disk/by-id/nvme-disk-b", "/dev/nvme2n1"].map(PathBuf::from));
    }

    #[test]
    fn a_pool_without_paths_or_config_lists_nothing() {
        assert!(zpool_data_devices("cannot open 'nope': no such pool\n").is_empty());
        // Without -P the rows are bare names, which aren't trusted as paths.
        assert!(zpool_data_devices("config:\n\n\tNAME STATE\n\ttank ONLINE\n\t  nvme0n1 ONLINE\n").is_empty());
    }

    #[test]
    fn one_spinning_disk_makes_the_pool_a_hard_disk() {
        assert_eq!(all_solid_state([Some(true), Some(true)].into_iter()), Some(true));
        assert_eq!(all_solid_state([Some(true), None, Some(false)].into_iter()), Some(false));
        assert_eq!(all_solid_state([Some(true), None].into_iter()), None, "an unreadable disk leaves it unknown");
        assert_eq!(all_solid_state(std::iter::empty()), None);
    }

    #[test]
    fn asking_about_a_missing_pool_or_device_never_panics() {
        assert_eq!(zfs_pool_solid_state("romexplorer-test-no-such-pool"), None);
        assert_eq!(block_device_solid_state(Path::new("/dev/romexplorer-test-no-such-device")), None);
    }
}
