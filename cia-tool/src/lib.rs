//! Native Rust decryption of Nintendo 3DS CIA and CCI (.3ds) files.
//!
//! A port of the logic in Batch CIA 3DS Decryptor Redux, without ctrtool,
//! makerom or decrypt.exe.

pub mod cci;
pub mod cia;
pub mod keys;
pub mod naming;
pub mod ncch;
pub mod process;
pub mod seeddb;
pub mod text;
#[cfg(feature = "testkit")]
pub mod testkit;

/// The error a conversion stops with when its progress callback returns `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}
