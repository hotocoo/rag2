use super::{ErrorLine, load};
use crate::api::{Session, to_yaml};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Value, json};

/// The whole profile as YAML (for anything the guided pages do not cover), plus version history
/// with activation and rollback.
#[component]
pub fn Config(s: Session) -> impl IntoView {
    let text = RwSignal::new(String::new());
    let versions = RwSignal::new(None::<Value>);
    let preview = RwSignal::new(None::<Value>);
    let comment = RwSignal::new(String::new());
    let err = RwSignal::new(None::<String>);
    let info = RwSignal::new(None::<String>);
    let reload = {
        let s = s.clone();
        move || {
            let s = s.clone();
            load(&s, "/v1/admin/config/versions", versions, err);
            spawn_local(async move {
                match s.get::<Value>("/v1/admin/config").await {
                    Ok(v) => text.set(to_yaml(&v["profile"])),
                    Err(e) => err.set(Some(e)),
                }
            });
        }
    };
    reload();
    let call = {
        let s = s.clone();
        let reload = reload.clone();
        move |path: String, body: Value, done: &'static str| {
            let (s, reload) = (s.clone(), reload.clone());
            spawn_local(async move {
                match s.post::<_, Value>(&path, &body).await {
                    Ok(v) => {
                        if path.ends_with("/preview") {
                            preview.set(Some(v));
                        } else {
                            preview.set(None);
                            info.set(Some(done.to_string()));
                            reload();
                        }
                        err.set(None);
                    }
                    Err(e) => err.set(Some(e)),
                }
            });
        }
    };
    let (c1, c2, c3, c4) = (call.clone(), call.clone(), call.clone(), call);
    view! {
        <h2>"Profile and versions"</h2>
        <ErrorLine err />
        {move || info.get().map(|i| view! { <p class="ok" role="status">{i}</p> })}
        <section class="panel">
            <label for="yaml">"Profile (YAML). Secrets stay references such as env:NAME."</label>
            <textarea id="yaml" class="code" spellcheck="false" prop:value=move || text.get() on:input=move |e| text.set(event_target_value(&e))></textarea>
            <div class="row" style="margin-top:12px">
                <div>
                    <label for="note">"Change note"</label>
                    <input id="note" prop:value=move || comment.get() on:input=move |e| comment.set(event_target_value(&e)) />
                </div>
                <div style="flex:0 0 auto;display:flex;gap:8px">
                    <button class="secondary" on:click=move |_| c1("/v1/admin/config/preview".into(), json!({"text": text.get_untracked()}), "")>"Validate and diff"</button>
                    <button class="primary" on:click=move |_| c2("/v1/admin/config/versions".into(), json!({"text": text.get_untracked(), "comment": comment.get_untracked(), "activate": true}), "Activated a new version.")>"Save and activate"</button>
                </div>
            </div>
            {move || preview.get().map(|p| view! {
                <p class=if p["valid"].as_bool().unwrap_or(false) { "ok" } else { "err" }>
                    {if p["valid"].as_bool().unwrap_or(false) { "Valid." } else { "Invalid; fix the issues below." }}
                    {format!(" {} change(s).", p["changes"].as_array().map_or(0, Vec::len))}
                </p>
                <pre>{serde_json::to_string_pretty(&json!({"changes": p["changes"], "issues": p["issues"]})).unwrap_or_default()}</pre>
            })}
        </section>
        <section class="panel">
            <h3>"History"</h3>
            <button class="secondary" on:click=move |_| c3("/v1/admin/config/rollback".into(), json!({}), "Rolled back to the previous version.")>"Roll back to previous"</button>
            {move || versions.get().map(|v| {
                let active = v["active"].as_u64().unwrap_or(0);
                let mut list = v["versions"].as_array().cloned().unwrap_or_default();
                list.reverse();
                let act = c4.clone();
                view! {
                    <table style="margin-top:12px">
                        <thead><tr><th>"Version"</th><th>"Author"</th><th>"Note"</th><th></th></tr></thead>
                        <tbody>
                            {list.into_iter().map(|m| {
                                let n = m["version"].as_u64().unwrap_or(0);
                                let act = act.clone();
                                view! {
                                    <tr>
                                        <td>{format!("v{n}")}{(n == active).then(|| view! { " " <span class="tag ok">"active"</span> })}</td>
                                        <td>{m["author"].as_str().unwrap_or("").to_string()}</td>
                                        <td>{m["comment"].as_str().unwrap_or("").to_string()}</td>
                                        <td>{(n != active).then(|| view! {
                                            <button class="secondary" on:click=move |_| act(format!("/v1/admin/config/versions/{n}/activate"), json!({}), "Activated.")>"Activate"</button>
                                        })}</td>
                                    </tr>
                                }
                            }).collect_view()}
                        </tbody>
                    </table>
                }
            })}
        </section>
    }
}
