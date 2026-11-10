//! chdlite: drop a disc image, get a CHD or an RVZ. Drop a cartridge dump, get it in the
//! header / byte order picked in the settings ([`cart`]).
//!
//! A thin front end over the romexplorer core: [`classify`] decides which container an input
//! wants (CD CHD, GD-ROM CHD, DVD CHD, or RVZ for GameCube/Wii), [`job`] runs one input
//! through `romexplorer::rom`'s chdman and Dolphin wrappers, and [`batch`] runs many, several
//! at a time. [`cli`] is the console front end shared by the `chdlite`, `chdcomp`, `chdread`
//! and `chdhash` binaries; the GUI (`chdlite-gui`) drives [`batch`] directly. On Android the
//! GUI is the whole app, and `android` is its bridge to the Java side.

#[cfg(target_os = "android")]
pub mod android;
pub mod batch;
pub mod cart;
pub mod classify;
pub mod cli;
pub mod job;
pub mod natives;
pub mod settings;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
