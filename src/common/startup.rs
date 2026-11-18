//! What start-up is doing right now: starting the database, each schema update, each data
//! upgrade, the worker. Reported here by the code doing it (`boot::prepare`,
//! `database::connection::migrate`, the server's `run`), logged, and read by a start screen
//! (the desktop app opens its window first and shows this line until it is ready).

use std::sync::RwLock;

static STEP: RwLock<Option<String>> = RwLock::new(None);

/// Says what start-up is doing now: logged, and kept for [`current`].
pub fn report(step: impl Into<String>) {
    let step = step.into();
    log::info!("start-up: {step}");
    *STEP.write().unwrap_or_else(|p| p.into_inner()) = Some(step);
}

/// The last step [`report`]ed, if start-up has said anything yet.
pub fn current() -> Option<String> {
    STEP.read().unwrap_or_else(|p| p.into_inner()).clone()
}
