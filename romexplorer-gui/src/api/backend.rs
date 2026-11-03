//! Where a request actually goes. Two implementations sit behind the one set of entry
//! points in [`super`]: [`super::rest`] speaks HTTP to a romexplorer_server (the web build,
//! mobile, and a desktop pointed at someone else's machine), and `super::local` -- desktop
//! only -- answers from this process against this machine's own database.
//!
//! The screens never see this. They keep calling `api::get` / `api::post` / ... with a
//! route string, which is what lets the backend swap without touching ~120 call sites.

use serde_json::Value;
use std::sync::OnceLock;

use super::ApiResult;

/// Futures here are deliberately **not** `Send`: on the web everything runs on one thread
/// and `reqwest`'s wasm futures aren't `Send` to begin with. The backend *object* is
/// `Send + Sync` because it lives in a static; the futures it hands back need not be.
pub type BoxFut<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;

/// The GUI's own verb list, so a backend never has to name `reqwest`'s types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    /// For building a request the in-process router can take.
    #[cfg_attr(not(feature = "local"), allow(dead_code))]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

pub trait Backend: Send + Sync {
    /// The response body as text, for the caller to parse -- into a `Value` for the screens
    /// that walk one, or straight into a typed struct (`super::get_as`), which is why this
    /// is not a `Value`: parsing to a tree and then to a struct would pay for both.
    fn call<'a>(&'a self, method: Method, path: &'a str, body: Option<&'a Value>) -> BoxFut<'a, ApiResult<String>>;
    /// This machine's own database, reached in-process: the only backend that may choose the
    /// instance mode (a server refuses it over REST).
    fn in_process(&self) -> bool {
        false
    }

    /// A multipart POST (one file plus text fields) -- separate because it is the one
    /// request whose body isn't JSON.
    fn upload<'a>(&'a self, path: &'a str, fields: Vec<(String, String)>, file_field: &'a str, file_name: String, bytes: Vec<u8>) -> BoxFut<'a, ApiResult>;

    /// A backup zip added by its path on this machine, without reading it into memory -- only
    /// a backend running in this process can; the rest return `None` and the caller uploads
    /// the bytes instead.
    fn add_backup_from_path<'a>(&'a self, _local_path: std::path::PathBuf, _file_name: String) -> Option<BoxFut<'a, ApiResult>> {
        None
    }

    /// A DAT file or bundle stored into the DAT folder by its path on this machine
    /// (`POST /api/v1/dats/upload`), the same way: in-process only, `None` elsewhere.
    fn add_dat_from_path<'a>(&'a self, _local_path: std::path::PathBuf, _file_name: String, _source: Option<String>) -> Option<BoxFut<'a, ApiResult>> {
        None
    }
}

static BACKEND: OnceLock<Box<dyn Backend>> = OnceLock::new();

/// Installs the backend. Called once at start-up by the desktop build (`boot`); every other
/// build leaves it unset and gets REST, which is the only thing they can use anyway.
#[cfg_attr(not(feature = "local"), allow(dead_code))]
pub fn set_backend(b: Box<dyn Backend>) {
    let _ = BACKEND.set(b);
}

pub fn backend() -> &'static dyn Backend {
    BACKEND.get_or_init(|| Box::new(super::rest::RestBackend)).as_ref()
}
