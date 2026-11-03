//! Config › Advanced (the console's `showAdvancedPage`): operations that can damage the
//! database or lose files, kept away from the everyday screens behind a loud warning. DB
//! optimize lives here; later repair or duplicate-delete jobs belong here too.

use dioxus::prelude::*;

use crate::api;
use crate::state;

#[component]
pub fn ConfigAdvanced() -> Element {
    if !api::is_admin() {
        return rsx! { div { class: "card muted", {state::tr("Only an administrator can use these operations.")} } };
    }
    rsx! {
        div { class: "card", style: "border:2px solid var(--bad)",
            h2 { class: "bad", {state::tr("Caution")} }
            p { class: "bad", style: "font-weight:bold", {state::tr("The operations on this page can corrupt the database or lose files. Make a backup first (Config › Backup), and use them only when you know why you need them.")} }
        }
        div { class: "card",
            h2 { {state::tr("Database optimize")} }
            p { class: "muted", {state::tr("Purges soft-deleted rows and rebuilds the tables. It also destroys the records that let moved and renamed files be recognised, so it is rarely a good idea; the confirmation says more.")} }
            div { class: "toolbar",
                button { class: "btn danger", r#type: "button", onclick: move |_| super::data::db_optimize_dialog(None), {state::tr("DB optimize (purge soft-deleted rows)…")} }
                crate::ui::QueueButton {}
            }
        }
    }
}
