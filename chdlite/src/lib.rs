//! chdlite: drop a disc image, get a CHD or an RVZ.
//!
//! A thin front end over the romexplorer core: [`classify`] decides which container an input
//! wants (CD CHD, GD-ROM CHD, DVD CHD, or RVZ for GameCube/Wii), [`job`] runs one input
//! through `romexplorer::rom`'s chdman and Dolphin wrappers, and [`batch`] runs many, several
//! at a time. [`cli`] is the console front end shared by the `chdlite`, `chdcomp`, `chdread`
//! and `chdhash` binaries; the GUI (`chdlite-gui`) drives [`batch`] directly.

pub mod batch;
pub mod classify;
pub mod cli;
pub mod job;
pub mod natives;
pub mod settings;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
