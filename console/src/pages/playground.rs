use super::ErrorLine;
use crate::api::{Session, seg};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Value, json};

/// Run a query as a client would and see the evidence, scores, route, confidence and timings.
#[component]
pub fn Playground(s: Session) -> impl IntoView {
    let tenant = RwSignal::new(String::new());
    let query = RwSignal::new(String::new());
    let result = RwSignal::new(None::<Value>);
    let err = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let run = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let s = s.clone();
        busy.set(true);
        spawn_local(async move {
            let path = format!("/v1/tenants/{}/query", seg(&tenant.get_untracked()));
            match s.post::<_, Value>(&path, &json!({ "query": query.get_untracked(), "no_cache": true })).await {
                Ok(v) => {
                    result.set(Some(v));
                    err.set(None);
                }
                Err(e) => err.set(Some(e)),
            }
            busy.set(false);
        });
    };
    view! {
        <h2>"Playground"</h2>
        <form class="panel row" on:submit=run>
            <div style="flex:0 1 200px">
                <label for="pt">"Tenant"</label>
                <input id="pt" prop:value=move || tenant.get() on:input=move |e| tenant.set(event_target_value(&e)) />
            </div>
            <div style="flex:3 1 300px">
                <label for="pq">"Query"</label>
                <input id="pq" prop:value=move || query.get() on:input=move |e| query.set(event_target_value(&e)) />
            </div>
            <div style="flex:0 0 auto"><button class="primary" type="submit" disabled=move || busy.get()>"Search"</button></div>
        </form>
        <ErrorLine err />
        {move || result.get().map(|r| {
            let abst = r["abstained"].clone();
            view! {
                <section class="panel">
                    <p>
                        {format!("Confidence {:.3}", r["confidence"].as_f64().unwrap_or(0.0))}
                        " · " {format!("route {}", r["route"]["retrievers"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" + ")).unwrap_or_default())} " (" {r["route"]["reason"].as_str().unwrap_or("").to_string()} ")"
                        " · " {format!("{} µs", r["timings"]["total_us"])}
                    </p>
                    {(!abst.is_null()).then(|| view! { <p class="warn">{format!("Abstained: {} ({})", abst["reason"], abst["detail"].as_str().unwrap_or(""))}</p> })}
                    {r["warnings"].as_array().cloned().unwrap_or_default().into_iter().map(|w| view! { <p class="warn">{w.as_str().unwrap_or("").to_string()}</p> }).collect_view()}
                    {r["contradictions"].as_array().cloned().unwrap_or_default().into_iter().map(|c| view! {
                        <p class="err">{format!("Contradiction ({}): chunk {} vs {}: {}", c["detector"].as_str().unwrap_or(""), c["a"], c["b"], c["reason"].as_str().unwrap_or(""))}</p>
                    }).collect_view()}
                </section>
                {r["evidence"].as_array().cloned().unwrap_or_default().into_iter().enumerate().map(|(i, e)| {
                    let p = e["provenance"].clone();
                    let signals = e["signals"].as_array().cloned().unwrap_or_default().iter().map(|s| format!("{}#{} ({:.3})", s["retriever"].as_str().unwrap_or(""), s["rank"], s["score"].as_f64().unwrap_or(0.0))).collect::<Vec<_>>().join(", ");
                    view! {
                        <article class="panel evidence">
                            <h3>{format!("[{}] {} · chunk {} · v{}", i + 1, p["doc_id"].as_str().unwrap_or(""), p["chunk_ordinal"], p["doc_version"])}</h3>
                            <p class="muted">{format!("score {:.4} · {signals}", e["score"].as_f64().unwrap_or(0.0))}</p>
                            <p>{e["text"].as_str().unwrap_or("").to_string()}</p>
                        </article>
                    }
                }).collect_view()}
            }
        })}
    }
}
