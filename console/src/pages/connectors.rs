use super::ErrorLine;
use crate::api::{Session, seg};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Value, json};

/// Sources synchronized on a schedule. Define them in the profile (`connectors:`); test the
/// connection and run a sync from here.
#[component]
pub fn Connectors(s: Session) -> impl IntoView {
    let rows = RwSignal::new(Vec::<Value>::new());
    let note = RwSignal::new(None::<(bool, String)>);
    let err = RwSignal::new(None::<String>);
    let reload = {
        let s = s.clone();
        move || {
            let s = s.clone();
            spawn_local(async move {
                match s.get::<Vec<Value>>("/v1/admin/connectors").await {
                    Ok(v) => rows.set(v),
                    Err(e) => err.set(Some(e)),
                }
            });
        }
    };
    reload();
    let act = {
        let s = s.clone();
        let reload = reload.clone();
        move |name: String, what: &'static str| {
            let (s, reload) = (s.clone(), reload.clone());
            spawn_local(async move {
                match s.post::<_, Value>(&format!("/v1/admin/connectors/{}/{what}", seg(&name)), &json!({})).await {
                    Ok(v) if what == "test" => note.set(Some((v["ok"].as_bool().unwrap_or(false), v["message"].as_str().unwrap_or("").to_string()))),
                    Ok(v) => note.set(Some((true, format!("{name}: {} created, {} updated, {} unchanged, {} deleted", v["created"], v["updated"], v["unchanged"], v["deleted"])))),
                    Err(e) => note.set(Some((false, e))),
                }
                reload();
            });
        }
    };
    view! {
        <h2>"Connectors"</h2>
        <p class="muted">"Add or change connectors in Profile & versions (the connectors section); every change is validated and versioned."</p>
        <ErrorLine err />
        {move || note.get().map(|(ok, m)| view! { <p class=if ok { "ok" } else { "err" } role="status">{m}</p> })}
        <section class="panel">
            <table>
                <thead><tr><th>"Name"</th><th>"Tenant"</th><th>"Kind"</th><th>"Every"</th><th>"Last sync"</th><th></th></tr></thead>
                <tbody>
                    {move || rows.get().into_iter().map(|c| {
                        let name = c["name"].as_str().unwrap_or("").to_string();
                        let st = &c["status"];
                        let last = match st["last_ok"].as_bool() {
                            None => view! { <span class="muted">"never"</span> }.into_any(),
                            Some(true) => view! { <span class="ok">{format!("ok · {} fetched", st["report"]["fetched"])}</span> }.into_any(),
                            Some(false) => view! { <span class="err">{st["message"].as_str().unwrap_or("failed").to_string()}</span> }.into_any(),
                        };
                        let (a1, a2) = (act.clone(), act.clone());
                        let (n1, n2) = (name.clone(), name.clone());
                        view! {
                            <tr>
                                <td><strong>{name.clone()}</strong></td>
                                <td>{c["tenant"].as_str().unwrap_or("").to_string()}</td>
                                <td>{c["kind"].as_str().unwrap_or("").to_string()}</td>
                                <td>{match c["interval_s"].as_u64() { Some(0) | None => "on demand".to_string(), Some(s) => format!("{s} s") }}</td>
                                <td>{last}</td>
                                <td style="white-space:nowrap">
                                    <button class="secondary" on:click=move |_| a1(n1.clone(), "test")>"Test connection"</button>
                                    " "
                                    <button class="primary" on:click=move |_| a2(n2.clone(), "run")>"Sync now"</button>
                                </td>
                            </tr>
                        }
                    }).collect_view()}
                </tbody>
            </table>
        </section>
    }
}
