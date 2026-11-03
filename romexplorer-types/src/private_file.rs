//! Files only their owner may read: the server token, the vault's master key, `database.env`,
//! the TLS private key and the GUI's `gui.json` (`private-files`).
//!
//! Unix gets mode 0o600. Windows gets a *protected* DACL -- nothing inherited from the parent
//! directory, which under `C:\Program Files` or `C:\ProgramData` would otherwise grant every
//! local user read access -- with full control for three SIDs: the current user, LocalSystem
//! and BUILTIN\Administrators. The last two are Windows' counterpart of root reading a 0600
//! file, and they are not optional: install.ps1 runs first-run setup as the (elevated) user who
//! installs, then the service runs as LocalSystem, which must still open the files setup wrote.
//! Both are resolved by well-known SID, never by name, because group names are localised.
//! Here rather than in the backend for the same reason as `pinning`: the GUI's native builds
//! need it without the backend crate.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Creates `path` (it must not exist) for writing, owner-only before anything is written to it.
pub fn create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(windows)]
    restrict(path)?;
    Ok(file)
}

/// Writes `bytes` to `path`, replacing it, and leaves it owner-only -- including a file an
/// older build wrote with default permissions.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Tighten before writing, so the secret never sits in a readable file.
    let mut file = options.open(path)?;
    restrict(path)?;
    file.write_all(bytes)
}

/// Makes an existing file owner-only.
pub fn restrict(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(windows)]
    {
        windows::restrict(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, ERROR_SUCCESS, HANDLE};
    use windows_sys::Win32::Security::Authorization::{
        SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, SET_ACCESS, SE_FILE_OBJECT,
        TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        CreateWellKnownSid, GetTokenInformation, TokenUser, WinBuiltinAdministratorsSid, WinLocalSystemSid, ACL,
        DACL_SECURITY_INFORMATION, NO_INHERITANCE, PROTECTED_DACL_SECURITY_INFORMATION, PSID, SECURITY_MAX_SID_SIZE, TOKEN_QUERY,
        TOKEN_USER, WELL_KNOWN_SID_TYPE,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// `FILE_ALL_ACCESS`, spelled out so this needs no Storage feature of windows-sys.
    const FILE_ALL_ACCESS: u32 = 0x001F_01FF;

    pub(super) fn restrict(path: &Path) -> io::Result<()> {
        let user = current_user()?;
        // TOKEN_USER is followed in the same buffer by the SID it points at.
        let user_sid = unsafe { (*(user.as_ptr() as *const TOKEN_USER)).User.Sid };
        let system = well_known(WinLocalSystemSid)?;
        let admins = well_known(WinBuiltinAdministratorsSid)?;
        let entry = |sid: PSID| EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_ALL_ACCESS,
            grfAccessMode: SET_ACCESS,
            grfInheritance: NO_INHERITANCE,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: sid as *mut u16,
            },
        };
        let entries = [entry(user_sid), entry(system.as_ptr() as PSID), entry(admins.as_ptr() as PSID)];
        let mut acl: *mut ACL = std::ptr::null_mut();
        let status = unsafe { SetEntriesInAclW(entries.len() as u32, entries.as_ptr(), std::ptr::null(), &mut acl) };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let status = unsafe {
            SetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl,
                std::ptr::null(),
            )
        };
        unsafe { LocalFree(acl as _) };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(())
    }

    /// A well-known SID (LocalSystem, BUILTIN\Administrators), in a buffer that outlives the ACL
    /// built from it.
    fn well_known(kind: WELL_KNOWN_SID_TYPE) -> io::Result<Vec<u64>> {
        let mut buf = vec![0u64; (SECURITY_MAX_SID_SIZE as usize).div_ceil(8)];
        let mut len = SECURITY_MAX_SID_SIZE;
        if unsafe { CreateWellKnownSid(kind, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(buf)
    }

    /// The process token's TOKEN_USER, as a u64-aligned buffer.
    fn current_user() -> io::Result<Vec<u64>> {
        let mut token: HANDLE = std::ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut len = 0u32;
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len) };
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        let ok = unsafe { GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) };
        let err = io::Error::last_os_error();
        unsafe { CloseHandle(token) };
        if ok == 0 {
            return Err(err);
        }
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("romexplorer-private-file-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn create_new_refuses_an_existing_file_and_write_replaces_it() {
        let dir = scratch("create");
        let path = dir.join("secret");
        std::io::Write::write_all(&mut create_new(&path).unwrap(), b"one").unwrap();
        assert_eq!(create_new(&path).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        write(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn unix_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode");
        let path = dir.join("secret");
        std::fs::write(&path, b"x").unwrap();
        write(&path, b"y").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reads the DACL back with icacls rather than a second hand-rolled FFI path, so the test
    /// checks what Windows reports, not what this module believes it set.
    #[cfg(windows)]
    #[test]
    fn windows_dacl_is_protected_and_owner_only() {
        let dir = scratch("dacl");
        let path = dir.join("secret");
        std::fs::write(&path, b"x").unwrap();
        write(&path, b"y").unwrap();
        let out = std::process::Command::new("icacls").arg(&path).output().expect("icacls runs");
        let text = String::from_utf8_lossy(&out.stdout);
        let user = std::env::var("USERNAME").unwrap();
        let aces: Vec<&str> = text
            .lines()
            .map(|l| l.trim().trim_start_matches(path.to_str().unwrap()).trim())
            .filter(|l| l.contains(":("))
            .collect();
        // The user, LocalSystem and Administrators -- and nobody else (no Users, no Everyone).
        // Checked by count rather than by group name, because icacls prints localised names.
        assert_eq!(aces.len(), 3, "exactly three ACEs: {text}");
        assert!(aces.iter().any(|a| a.to_lowercase().contains(&user.to_lowercase())), "one ACE is the current user's: {text}");
        assert!(aces.iter().all(|a| a.contains("(F)")), "full control each: {text}");
        assert!(aces.iter().all(|a| !a.contains("(I)")), "nothing inherited: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
