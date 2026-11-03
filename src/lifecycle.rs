//! How a romexplorer process learns it is time to stop, and what it owes a service manager
//! when it does.
//!
//! Both of these used to be assumptions rather than code, and both were wrong once the app
//! started being packaged:
//!
//! * Shutdown listened for `ctrl_c` -- SIGINT -- and nothing else. systemd, launchd and
//!   `docker stop` all send **SIGTERM**, so the process died where it stood: running jobs
//!   were never put back to `pending`, and `database::server::MariaDbServer::shutdown` never
//!   ran, which means the bundled MariaDB was killed mid-write and InnoDB crash-recovered on
//!   the next boot. Every reboot. [`stop_requested`] is the fix.
//!
//! * A restart spawned a **child** copy of this executable. Under systemd that child is in
//!   the same cgroup and is killed with the unit; under launchd it is not adopted as the
//!   job; either way you can end up with two servers wanting port 7337. Under a service
//!   manager the right move is to exit and let it start us again, which is what
//!   [`service_managed`] and [`RESTART_EXIT_CODE`] are for.

/// The exit status that asks a service manager to start us again.
///
/// 75 is `EX_TEMPFAIL`. systemd needs it named in `RestartForceExitStatus=` (or
/// `Restart=always`); launchd's `KeepAlive` restarts any non-zero exit by default.
pub const RESTART_EXIT_CODE: i32 = 75;

/// Whether something else owns this process's lifetime, set by the unit file, the launchd
/// plist, the WinSW service definition and the container image.
pub fn service_managed() -> bool {
    matches!(std::env::var("ROMEXPLORER_SERVICE_MANAGED").as_deref(), Ok("1") | Ok("true") | Ok("yes"))
}

/// Resolves when the operating system asks this process to stop.
///
/// Unix: SIGINT (ctrl-c) or SIGTERM. Windows: ctrl-c, or the console being closed, or the
/// user logging off, or the machine shutting down -- note that Windows allows only about
/// five seconds for the last three, which is not enough for a full teardown, and is the
/// reason the Windows service wants a real Service Control Manager handler rather than
/// relying on console events.
pub async fn stop_requested() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        // A failure to install the handler must not mean "stop immediately", so fall back to
        // a future that never resolves and let ctrl-c carry the process.
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("couldn't listen for SIGTERM ({e}); only ctrl-c will stop this cleanly");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows;
        let mut close = windows::ctrl_close().expect("ctrl_close handler");
        let mut logoff = windows::ctrl_logoff().expect("ctrl_logoff handler");
        let mut sd = windows::ctrl_shutdown().expect("ctrl_shutdown handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = close.recv() => {}
            _ = logoff.recv() => {}
            _ = sd.recv() => {}
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_managed_reads_the_usual_spellings() {
        for (value, want) in [("1", true), ("true", true), ("yes", true), ("0", false), ("", false)] {
            std::env::set_var("ROMEXPLORER_SERVICE_MANAGED", value);
            assert_eq!(service_managed(), want, "{value:?}");
        }
        std::env::remove_var("ROMEXPLORER_SERVICE_MANAGED");
        assert!(!service_managed(), "unset means nobody is managing us");
    }
}
