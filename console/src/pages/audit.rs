use super::ErrorLine;
use crate::api::Session;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;

#[component]
pub fn Audit(s: Session) -> impl IntoView {
    let events = RwSignal::new(Vec::<Value>::new());
    let next = RwSignal::new(None::<String>);
    let err = RwSignal::new(None::<String>);
    let fetch = move |after: Option<String>| {
        let s = s.clone();
        spawn_local(async move {
            let path = match &after {
                Some(c) => format!("/v1/admin/audit?after={c}"),
                None => "/v1/admin/audit".to_string(),
            };
            match s.get::<Value>(&path).await {
                Ok(v) => {
                    events.update(|e| e.extend(v["events"].as_array().cloned().unwrap_or_default()));
                    next.set(v["next"].as_str().map(String::from));
                }
                Err(e) => err.set(Some(e)),
            }
        });
    };
    fetch(None);
    view! {
        <h2>"Audit log"</h2>
        <ErrorLine err />
        <section class="panel">
            <table>
                <thead><tr><th>"Time (UTC ms)"</th><th>"Principal"</th><th>"Action"</th><th>"Tenant"</th><th>"Target"</th><th>"Outcome"</th></tr></thead>
                <tbody>
                    {move || events.get().into_iter().map(|e| view! {
                        <tr>
                            <td>{e["at_ms"].to_string()}</td>
                            <td>{e["principal"].as_str().unwrap_or("").to_string()}</td>
                            <td>{e["action"].as_str().unwrap_or("").to_string()}</td>
                            <td>{e["tenant"].as_str().unwrap_or("").to_string()}</td>
                            <td>{e["target"].as_str().unwrap_or("").to_string()}</td>
                            <td class=if e["outcome"] == "ok" { "ok" } else { "warn" }>{e["outcome"].as_str().unwrap_or("").to_string()}</td>
                        </tr>
                    }).collect_view()}
                </tbody>
            </table>
            <button class="secondary" style="margin-top:12px" disabled=move || next.get().is_none()
                on:click=move |_| fetch(next.get_untracked())>"More events"</button>
        </section>
    }
}
