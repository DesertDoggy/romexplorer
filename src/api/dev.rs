//! Development exports for sharing with other developers.

use crate::database::counterparts;

use super::{App, Error, Result};

/// The release -> USA / English counterpart table as CSV: `full` (DAT names) or `stripped`
/// (region / language / revision tags removed, duplicates collapsed).
pub async fn counterparts_csv(app: &App, kind: &str) -> Result<String> {
    let rows = counterparts::build(&app.pool).await?;
    match kind {
        "full" => Ok(counterparts::to_csv(&rows)),
        "stripped" => Ok(counterparts::to_csv(&counterparts::stripped(&rows))),
        other => Err(Error::invalid(format!("unknown table {other:?} (full, stripped)"))),
    }
}
