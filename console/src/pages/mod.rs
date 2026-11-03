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
use crate::copy::{change as t, common};
use crate::ui::{Alert, Button, Card, Field, InputFrame, Table, TextInput, Tone, Variant};
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

/// A server error, as a danger alert.
#[component]
pub fn ErrorLine(err: RwSignal<Option<String>>) -> impl IntoView {
    move || {
        err.get()
            .map(|e| view! { <Alert tone=Tone::Danger title=common::ERROR_TITLE>{e}</Alert> })
    }
}

/// A string field of a JSON value, or empty.
pub fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// Preview a candidate profile against the running one, then apply it as a new active version.
#[component]
pub fn ChangeFlow(
    s: Session,
    candidate: RwSignal<Option<Value>>,
    on_applied: Callback<()>,
) -> impl IntoView {
    let preview = RwSignal::new(None::<Value>);
    let comment = RwSignal::new(String::new());
    let err = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let s2 = s.clone();
    let do_preview = move |_| {
        let Some(p) = candidate.get_untracked() else {
            return;
        };
        let s = s.clone();
        busy.set(true);
        spawn_local(async move {
            match s
                .post::<_, Value>("/v1/admin/config/preview", &json!({ "profile": p }))
                .await
            {
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
        let Some(p) = candidate.get_untracked() else {
            return;
        };
        let s = s2.clone();
        busy.set(true);
        spawn_local(async move {
            let body =
                json!({ "profile": p, "comment": comment.get_untracked(), "activate": true });
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
    let side = |c: &Value, key: &str, op: &str| {
        c.get(key)
            .or(c.get("value").filter(|_| c["op"] == op))
            .map(|v| v.to_string())
            .unwrap_or_default()
    };
    view! {
        <Card eyebrow=t::LABEL label=t::LABEL>
            <div class="hs-stack">
                <div class="hs-cluster console-cluster--form">
                    <Field id="comment" label=t::NOTE hint=t::NOTE_HINT class="console-grow">
                        <TextInput id="comment" value=comment />
                    </Field>
                    <Button icon="eye" busy=busy disabled=Signal::derive(move || busy.get() || candidate.get().is_none())
                        on:click=do_preview>{t::PREVIEW}</Button>
                </div>
                {move || preview.get().map(|p| {
                    let valid = p["valid"].as_bool().unwrap_or(false);
                    let changes = p["changes"].as_array().cloned().unwrap_or_default();
                    let issues = p["issues"].as_array().cloned().unwrap_or_default();
                    let apply = apply.clone();
                    view! {
                        <h3 class="heading-4">{t::count(changes.len())}</h3>
                        <Table>
                            <thead><tr><th scope="col">{t::FIELD}</th><th scope="col">{t::BEFORE}</th><th scope="col">{t::AFTER}</th></tr></thead>
                            <tbody>
                                {changes.into_iter().map(|c| view! {
                                    <tr>
                                        <td class="console-mono">{text(&c["path"])}</td>
                                        <td class="console-mono">{side(&c, "before", "remove")}</td>
                                        <td class="console-mono">{side(&c, "after", "add")}</td>
                                    </tr>
                                }).collect_view()}
                            </tbody>
                        </Table>
                        {issues.into_iter().map(|i| {
                            let tone = if i["severity"] == "error" { Tone::Danger } else { Tone::Warning };
                            view! { <Alert tone>{t::issue(i["path"].as_str().unwrap_or_default(), i["message"].as_str().unwrap_or_default())}</Alert> }
                        }).collect_view()}
                        <div class="hs-cluster">
                            <Button variant=Variant::Primary icon="check" busy=busy
                                disabled=Signal::derive(move || busy.get() || !valid) on:click=apply>{t::APPLY}</Button>
                        </div>
                    }
                })}
                <ErrorLine err />
            </div>
        </Card>
    }
}

/// A select bound to `on_change`, with options `(value, label)`.
#[component]
pub fn SelectField(
    id: &'static str,
    label: &'static str,
    #[prop(into)] options: Signal<Vec<(String, String)>>,
    #[prop(into)] selected: Signal<String>,
    on_change: Callback<String>,
) -> impl IntoView {
    view! {
        <Field id=id label=label>
            <InputFrame frame=crate::ui::Frame::Select>
                <select id=id class="hs-input__control" on:change=move |e| on_change.run(event_target_value(&e))>
                    {move || options.get().into_iter().map(|(v, l)| {
                        let is = v == selected.get();
                        view! { <option value=v selected=is>{l}</option> }
                    }).collect_view()}
                </select>
            </InputFrame>
        </Field>
    }
}
