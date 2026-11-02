use super::pipelines::set_at;
use super::{ChangeFlow, ErrorLine};
use crate::api::{Session, seg};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Value, json};

/// Defaults written for a newly added tenant override; the server validates them.
const NEW_TENANT: &str = r#"{"requests_per_second": 100, "burst": 200, "max_documents": 1000000, "enabled": true}"#;

#[component]
pub fn Tenants(s: Session) -> impl IntoView {
    let candidate = RwSignal::new(None::<Value>);
    let live = RwSignal::new(Vec::<String>::new());
    let new_id = RwSignal::new(String::new());
    let confirm = RwSignal::new(String::new());
    let err = RwSignal::new(None::<String>);
    let reload = {
        let s = s.clone();
        move || {
            let s = s.clone();
            spawn_local(async move {
                match s.get::<Value>("/v1/admin/config").await {
                    Ok(v) => candidate.set(Some(v["profile"].clone())),
                    Err(e) => err.set(Some(e)),
                }
                match s.get::<Vec<String>>("/v1/admin/tenants").await {
                    Ok(t) => live.set(t),
                    Err(e) => err.set(Some(e)),
                }
            });
        }
    };
    reload();
    let rows = move || {
        let configured: Vec<String> =
            candidate.with(|c| c.as_ref().and_then(|c| c["tenants"].as_object().map(|o| o.keys().cloned().collect())).unwrap_or_default());
        let mut all: Vec<String> = configured.into_iter().chain(live.get()).collect();
        all.sort();
        all.dedup();
        all
    };
    let field = move |t: String, f: &'static str| {
        let at = vec!["tenants".to_string(), t.clone(), f.to_string()];
        let shown = move || {
            candidate.with(|c| {
                c.as_ref()
                    .map(|c| {
                        let v = &c["tenants"][t.as_str()][f];
                        if v.is_null() { c["tenant_defaults"][f].to_string() } else { v.to_string() }
                    })
                    .unwrap_or_default()
            })
        };
        view! {
            <input type="number" aria-label=f prop:value=shown on:change=move |e| {
                if let Ok(n) = event_target_value(&e).parse::<u64>() {
                    candidate.update(|c| if let Some(c) = c {
                        let id = at[1].clone();
                        if c["tenants"][id.as_str()].is_null() {
                            set_at(c, &["tenants".into(), id.clone()], serde_json::from_str(NEW_TENANT).unwrap_or(json!({})));
                        }
                        set_at(c, &at, json!(n));
                    });
                }
            } />
        }
    };
    let s_del = s.clone();
    let reload2 = reload.clone();
    let delete = move |t: String| {
        let s = s_del.clone();
        let reload = reload2.clone();
        if confirm.get_untracked() != t {
            err.set(Some(format!("Type \"{t}\" in the confirmation box to delete it and all its data.")));
            return;
        }
        spawn_local(async move {
            match s.delete(&format!("/v1/admin/tenants/{}?confirm={}", seg(&t), seg(&t))).await {
                Ok(_) => {
                    confirm.set(String::new());
                    reload();
                }
                Err(e) => err.set(Some(e)),
            }
        });
    };
    let on_applied = Callback::new(move |_| reload());
    view! {
        <h2>"Tenants and quotas"</h2>
        <ErrorLine err />
        <section class="panel">
            <table>
                <thead><tr><th>"Tenant"</th><th>"Requests/s"</th><th>"Burst"</th><th>"Max documents"</th><th>"Has data"</th><th></th></tr></thead>
                <tbody>
                    {move || rows().into_iter().map(|t| {
                        let has = live.get().contains(&t);
                        let del = delete.clone();
                        let tid = t.clone();
                        view! {
                            <tr>
                                <td><strong>{t.clone()}</strong></td>
                                <td>{field(t.clone(), "requests_per_second")}</td>
                                <td>{field(t.clone(), "burst")}</td>
                                <td>{field(t.clone(), "max_documents")}</td>
                                <td>{if has { "yes" } else { "no" }}</td>
                                <td>{has.then(|| view! { <button class="danger" on:click=move |_| del(tid.clone())>"Delete data"</button> })}</td>
                            </tr>
                        }
                    }).collect_view()}
                </tbody>
            </table>
            <div class="row" style="margin-top:12px">
                <div>
                    <label for="newt">"Add a tenant override"</label>
                    <input id="newt" placeholder="tenant-id" prop:value=move || new_id.get() on:input=move |e| new_id.set(event_target_value(&e)) />
                </div>
                <div style="flex:0 0 auto">
                    <button class="secondary" on:click=move |_| {
                        let id = new_id.get_untracked().trim().to_string();
                        if id.is_empty() { return; }
                        candidate.update(|c| if let Some(c) = c {
                            set_at(c, &["tenants".into(), id.clone()], serde_json::from_str(NEW_TENANT).unwrap_or(json!({})));
                        });
                        new_id.set(String::new());
                    }>"Add"</button>
                </div>
                <div>
                    <label for="confirm">"Confirm deletion by typing the tenant id"</label>
                    <input id="confirm" prop:value=move || confirm.get() on:input=move |e| confirm.set(event_target_value(&e)) />
                </div>
            </div>
        </section>
        <ChangeFlow s=s.clone() candidate on_applied />
    }
}
