use super::{ErrorLine, load};
use crate::api::Session;
use leptos::prelude::*;
use serde_json::Value;

#[component]
pub fn Overview(s: Session) -> impl IntoView {
    let cfg = RwSignal::new(None::<Value>);
    let tenants = RwSignal::new(None::<Value>);
    let comps = RwSignal::new(None::<Value>);
    let err = RwSignal::new(None::<String>);
    load(&s, "/v1/admin/config", cfg, err);
    load(&s, "/v1/admin/tenants", tenants, err);
    load(&s, "/v1/admin/components", comps, err);
    let stat = |label: &'static str, v: String| view! { <div class="panel stat"><label>{label}</label><div class="v">{v}</div></div> };
    view! {
        <h2>"Overview"</h2>
        <ErrorLine err />
        <div class="stats">
            {move || stat("Active version", cfg.get().map(|c| format!("v{}", c["version"])).unwrap_or("…".into()))}
            {move || stat("Profile", cfg.get().map(|c| c["name"].as_str().unwrap_or("").to_string()).unwrap_or("…".into()))}
            {move || stat("Tenants", tenants.get().and_then(|t| t.as_array().map(|a| a.len().to_string())).unwrap_or("…".into()))}
            {move || stat("Role", cfg.get().map(|c| c["profile"]["cluster"]["role"].as_str().unwrap_or("standalone").to_string()).unwrap_or("…".into()))}
        </div>
        <section class="panel">
            <h3>"Components"</h3>
            {move || comps.get().map(|c| view! {
                <table>
                    <thead><tr><th>"Kind"</th><th>"Name"</th><th>"Status"</th></tr></thead>
                    <tbody>
                        {c["embedders"].as_array().cloned().unwrap_or_default().into_iter().map(|e| view! {
                            <tr><td>"embedder"</td><td>{e.as_str().unwrap_or("").to_string()}</td><td class="ok">"loaded"</td></tr>
                        }).collect_view()}
                        {c["decision_models"].as_array().cloned().unwrap_or_default().into_iter().map(|d| {
                            let healthy = d[1].as_bool().unwrap_or(false);
                            view! {
                                <tr>
                                    <td>"decision model"</td>
                                    <td>{d[0].as_str().unwrap_or("").to_string()}</td>
                                    <td class=if healthy { "ok" } else { "err" }>{if healthy { "healthy" } else { "unreachable" }}</td>
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            })}
        </section>
    }
}
