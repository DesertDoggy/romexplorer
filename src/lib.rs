pub mod api;
pub mod common;
pub mod database;
pub mod lifecycle;
pub mod rom;
pub mod server;
pub mod worker;

pub use rom::archive::archive;
pub use rom::convert::nes;
pub use rom::extract::extract;
pub use rom::hash::hash;
pub use rom::native_lib::{detect_capabilities, LibVariant, NativeCapabilities};

// The per-game operation flow (detect -> extract -> hash -> convert -> archive in one pass)
// and what it takes/returns.
pub use common::models::{CueSheetStyle, FileData};
pub use rom::detect::{metadata_cycle, resolve_candidates, CycleOutcome, CycleStep};
pub use rom::models::{ArchiveArgs, ConvertArgs, MetadataArgs};
pub use rom::multiple::{Job, Limits, Multiple};
pub use rom::operation::{
    CostModel, ProgressEvent, Single, SingleArgs, SingleError, SingleOutcome, SkipReason, Stage, StageResult, StageResults,
    StageSwitches,
};
