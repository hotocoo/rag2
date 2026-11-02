//! Console pages. Guided editors change a copy of the active profile; every change goes through
//! the server's preview (validation + diff) before it can be applied as a new version.

mod audit;
mod config;
mod connectors;
mod documents;
mod overview;
mod pipelines;
mod playground;
mod tenants;

pub use audit::Audit;
pub use config::Config;
pub use connectors::Connectors;
pub use documents::Documents;
pub use overview::Overview;
pub use pipelines::Pipelines;
pub use playground::Playground;
pub use tenants::Tenants;

use crate::api::Session;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Value, json};

/// Fetch `path` once into `into`; failures land in `err`.
pub fn load(s: &Session, path: &str, into: RwSignal<Option<Value>>, err: RwSignal<Option<String>>) {
    let (s, path) = (s.clone(), path.to_string());
    spawn_local(async move {
        match s.get::<Value>(&path).await {
            Ok(v) => into.set(Some(v)),
            Err(e) => err.set(Some(e)),
        }
    });
}

#[component]
pub fn ErrorLine(err: RwSignal<Option<String>>) -> impl IntoView {
    move || err.get().map(|e| view! { <p class="err" role="alert">{e}</p> })
}

/// Preview a candidate profile against the running one, then apply it as a new active version.
#[component]
pub fn ChangeFlow(s: Session, candidate: RwSignal<Option<Value>>, on_applied: Callback<()>) -> impl IntoView {
    let preview = RwSignal::new(None::<Value>);
    let comment = RwSignal::new(String::new());
    let err = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let s2 = s.clone();
    let do_preview = move |_| {
        let Some(p) = candidate.get_untracked() else { return };
        let s = s.clone();
        busy.set(true);
        spawn_local(async move {
            match s.post::<_, Value>("/v1/admin/config/preview", &json!({ "profile": p })).await {
                Ok(v) => {
                    preview.set(Some(v));
                    err.set(None);
                }
                Err(e) => err.set(Some(e)),
            }
            busy.set(false);
        });
    };
    let apply = move |_| {
        let Some(p) = candidate.get_untracked() else { return };
        let s = s2.clone();
        busy.set(true);
        spawn_local(async move {
            let body = json!({ "profile": p, "comment": comment.get_untracked(), "activate": true });
            match s.post::<_, Value>("/v1/admin/config/versions", &body).await {
                Ok(_) => {
                    preview.set(None);
                    err.set(None);
                    on_applied.run(());
                }
                Err(e) => err.set(Some(e)),
            }
            busy.set(false);
        });
    };
    view! {
        <section class="panel" aria-label="Review and apply">
            <div class="row">
                <div>
                    <label for="comment">"Change note (recorded in version history)"</label>
                    <input id="comment" prop:value=move || comment.get() on:input=move |e| comment.set(event_target_value(&e)) />
                </div>
                <div style="flex:0 0 auto">
                    <button class="secondary" disabled=move || busy.get() || candidate.get().is_none() on:click=do_preview>
                        "Preview changes"
                    </button>
                </div>
            </div>
            {move || preview.get().map(|p| {
                let valid = p["valid"].as_bool().unwrap_or(false);
                let changes = p["changes"].as_array().cloned().unwrap_or_default();
                let issues = p["issues"].as_array().cloned().unwrap_or_default();
                view! {
                    <h3 style="margin-top:16px">{format!("{} change(s)", changes.len())}</h3>
                    <table>
                        <thead><tr><th>"Field"</th><th>"Before"</th><th>"After"</th></tr></thead>
                        <tbody>
                            {changes.into_iter().map(|c| view! {
                                <tr>
                                    <td><code>{c["path"].as_str().unwrap_or("").to_string()}</code></td>
                                    <td>{c.get("before").or(c.get("value").filter(|_| c["op"] == "remove")).map(|v| v.to_string()).unwrap_or_default()}</td>
                                    <td>{c.get("after").or(c.get("value").filter(|_| c["op"] == "add")).map(|v| v.to_string()).unwrap_or_default()}</td>
                                </tr>
                            }).collect_view()}
                        </tbody>
                    </table>
                    {issues.into_iter().map(|i| {
                        let cls = if i["severity"] == "error" { "err" } else { "warn" };
                        view! { <p class=cls>{format!("{}: {}", i["path"].as_str().unwrap_or(""), i["message"].as_str().unwrap_or(""))}</p> }
                    }).collect_view()}
                    <button class="primary" style="margin-top:12px" disabled=move || busy.get() || !valid on:click=apply.clone()>
                        "Apply as new active version"
                    </button>
                }
            })}
            <ErrorLine err />
        </section>
    }
}
