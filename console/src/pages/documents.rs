use super::ErrorLine;
use crate::api::{Session, seg};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;

/// Documents per page in the browser.
const PAGE: usize = 50;

#[component]
pub fn Documents(s: Session) -> impl IntoView {
    let tenant = RwSignal::new(String::new());
    let docs = RwSignal::new(Vec::<Value>::new());
    let chunks = RwSignal::new(None::<(String, Vec<Value>)>);
    let stats = RwSignal::new(None::<Value>);
    let err = RwSignal::new(None::<String>);
    let s1 = s.clone();
    let list = move |after: Option<String>| {
        let s = s1.clone();
        spawn_local(async move {
            let t = seg(&tenant.get_untracked());
            let mut path = format!("/v1/tenants/{t}/documents?limit={PAGE}");
            if let Some(a) = &after {
                path.push_str(&format!("&after={}", seg(a)));
            }
            match s.get::<Vec<Value>>(&path).await {
                Ok(v) => {
                    docs.set(v);
                    err.set(None);
                }
                Err(e) => err.set(Some(e)),
            }
            if let Ok(st) = s.get::<Value>(&format!("/v1/tenants/{t}/stats")).await {
                stats.set(Some(st));
            }
        });
    };
    let (l1, l2) = (list.clone(), list);
    let s2 = s.clone();
    let show = move |id: String| {
        let s = s2.clone();
        spawn_local(async move {
            let path = format!("/v1/tenants/{}/documents/{}/chunks", seg(&tenant.get_untracked()), seg(&id));
            match s.get::<Vec<Value>>(&path).await {
                Ok(c) => chunks.set(Some((id, c))),
                Err(e) => err.set(Some(e)),
            }
        });
    };
    view! {
        <h2>"Documents"</h2>
        <form class="panel row" on:submit=move |ev| { ev.prevent_default(); l1(None); }>
            <div><label for="dt">"Tenant"</label><input id="dt" prop:value=move || tenant.get() on:input=move |e| tenant.set(event_target_value(&e)) /></div>
            <div style="flex:0 0 auto"><button class="primary" type="submit">"List"</button></div>
        </form>
        <ErrorLine err />
        {move || stats.get().map(|st| view! {
            <p class="muted">{format!("{} documents · generation {} · lexical {} chunks · vector indexes {}", st["documents"], st["generation"], st["lexical_docs"], st["vector_indexes"])}</p>
        })}
        <section class="panel">
            <table>
                <thead><tr><th>"Id"</th><th>"Title"</th><th>"Version"</th><th>"Chunks"</th><th></th></tr></thead>
                <tbody>
                    {move || docs.get().into_iter().map(|d| {
                        let id = d["id"].as_str().unwrap_or("").to_string();
                        let sh = show.clone();
                        let idc = id.clone();
                        view! {
                            <tr>
                                <td><code>{id.clone()}</code></td>
                                <td>{d["title"].as_str().unwrap_or("").to_string()}</td>
                                <td>{d["version"].to_string()}</td>
                                <td>{d["chunks"].as_array().map_or(0, Vec::len)}</td>
                                <td><button class="secondary" on:click=move |_| sh(idc.clone())>"Chunks"</button></td>
                            </tr>
                        }
                    }).collect_view()}
                </tbody>
            </table>
            <button class="secondary" style="margin-top:12px" disabled=move || docs.with(|d| d.len() < PAGE)
                on:click=move |_| { let last = docs.with(|d| d.last().and_then(|x| x["id"].as_str().map(String::from))); l2(last); }>"Next page"</button>
        </section>
        {move || chunks.get().map(|(id, cs)| view! {
            <section class="panel">
                <h3>{format!("Chunks of {id}")}</h3>
                {cs.into_iter().map(|c| view! { <article class="evidence"><p class="muted">{format!("#{} · id {}", c["ordinal"], c["id"])}</p><p>{c["text"].as_str().unwrap_or("").to_string()}</p></article> }).collect_view()}
            </section>
        })}
    }
}
