use super::{ChangeFlow, ErrorLine};
use crate::api::Session;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Value, json};

/// Set `path` (object keys or array indices) inside `v`.
pub fn set_at(v: &mut Value, path: &[String], val: Value) {
    let mut cur = v;
    for p in path {
        cur = match p.parse::<usize>() {
            Ok(i) if cur.is_array() => &mut cur[i],
            _ => &mut cur[p.as_str()],
        };
    }
    *cur = val;
}

fn path(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// A numeric input bound to `path` in the candidate profile.
#[component]
fn Num(
    label: &'static str,
    candidate: RwSignal<Option<Value>>,
    at: Vec<String>,
    #[prop(optional)] float: bool,
    /// Table cells: the column header names the field, so the label is screen-reader only.
    #[prop(optional)]
    compact: bool,
) -> impl IntoView {
    let read = at.clone();
    let id = format!("f-{}", at.join("-"));
    view! {
        <div>
            {(!compact).then(|| view! { <label for=id.clone()>{label}</label> })}
            <input id=id aria-label=label type="number" step=if float { "0.01" } else { "1" }
                prop:value=move || candidate.with(|c| c.as_ref().map(|c| {
                    let mut v = c;
                    for p in &read {
                        v = match p.parse::<usize>() { Ok(i) if v.is_array() => &v[i], _ => &v[p.as_str()] };
                    }
                    match v.as_f64() {
                        // Thresholds are f32 in the server; print the shortest form that round-trips.
                        Some(x) if float => format!("{}", x as f32),
                        _ if v.is_null() => String::new(),
                        _ => v.to_string(),
                    }
                }).unwrap_or_default())
                on:change=move |e| {
                    let raw = event_target_value(&e);
                    let val = if float {
                        raw.parse::<f64>().ok().map(|x| json!(x))
                    } else {
                        raw.parse::<u64>().ok().map(|x| json!(x))
                    };
                    if let Some(v) = val {
                        candidate.update(|c| if let Some(c) = c { set_at(c, &at, v) });
                    }
                } />
        </div>
    }
}

#[component]
pub fn Pipelines(s: Session) -> impl IntoView {
    let candidate = RwSignal::new(None::<Value>);
    let selected = RwSignal::new(String::new());
    let err = RwSignal::new(None::<String>);
    let reload = {
        let s = s.clone();
        move || {
            let s = s.clone();
            spawn_local(async move {
                match s.get::<Value>("/v1/admin/config").await {
                    Ok(v) => {
                        let p = v["profile"].clone();
                        if selected.get_untracked().is_empty() {
                            selected.set(p["default_pipeline"].as_str().unwrap_or("default").to_string());
                        }
                        candidate.set(Some(p));
                    }
                    Err(e) => err.set(Some(e)),
                }
            });
        }
    };
    reload();
    let names = move || {
        candidate.with(|c| c.as_ref().and_then(|c| c["pipelines"].as_object().map(|o| o.keys().cloned().collect::<Vec<_>>())).unwrap_or_default())
    };
    let on_applied = Callback::new(move |_| reload());
    view! {
        <h2>"Pipelines"</h2>
        <p class="muted">"Edit retrieval behaviour without code. Changes are previewed and validated by the server before they become a new version."</p>
        <ErrorLine err />
        <section class="panel">
            <label for="pipeline">"Pipeline"</label>
            <select id="pipeline" on:change=move |e| selected.set(event_target_value(&e))>
                {move || names().into_iter().map(|n| {
                    let is = n == selected.get();
                    view! { <option value=n.clone() selected=is>{n.clone()}</option> }
                }).collect_view()}
            </select>
        </section>
        {move || {
            let name = selected.get();
            if name.is_empty() || candidate.with(|c| c.is_none()) {
                return view! { <p class="muted">"Loading…"</p> }.into_any();
            }
            let base = |rest: &[&str]| { let mut v = path(&["pipelines", name.as_str()]); v.extend(path(rest)); v };
            let retrievers = candidate.with(|c| c.as_ref().map(|c| c["pipelines"][name.as_str()]["retrievers"].as_array().cloned().unwrap_or_default()).unwrap_or_default());
            let fusion = candidate.with(|c| c.as_ref().map(|c| c["pipelines"][name.as_str()]["fusion"]["method"].as_str().unwrap_or("rrf").to_string()).unwrap_or_default());
            let fusion_at = base(&["fusion", "method"]);
            view! {
                <section class="panel">
                    <h3>"Retrievers"</h3>
                    <table>
                        <thead><tr><th>"Kind"</th><th>"Enabled"</th><th>"Weight"</th><th>"Candidates (top_k)"</th></tr></thead>
                        <tbody>
                            {retrievers.into_iter().enumerate().map(|(i, r)| {
                                let idx = i.to_string();
                                let enabled_at = base(&["retrievers", idx.as_str(), "enabled"]);
                                let on = r["enabled"].as_bool().unwrap_or(true);
                                view! {
                                    <tr>
                                        <td>{r["kind"].as_str().unwrap_or("").to_string()}{r["embedder"].as_str().map(|e| format!(" ({e})")).unwrap_or_default()}</td>
                                        <td><input type="checkbox" aria-label="enabled" prop:checked=on
                                            on:change=move |e| { let v = event_target_checked(&e); candidate.update(|c| if let Some(c) = c { set_at(c, &enabled_at, json!(v)) }); } /></td>
                                        <td><Num label="Weight" compact=true candidate at=base(&["retrievers", idx.as_str(), "weight"]) float=true /></td>
                                        <td><Num label="Candidates" compact=true candidate at=base(&["retrievers", idx.as_str(), "top_k"]) /></td>
                                    </tr>
                                }
                            }).collect_view()}
                        </tbody>
                    </table>
                </section>
                <section class="panel">
                    <h3>"Ranking and results"</h3>
                    <div class="row">
                        <div>
                            <label for="fusion">"Fusion"</label>
                            <select id="fusion" on:change=move |e| { let v = event_target_value(&e); candidate.update(|c| if let Some(c) = c { set_at(c, &fusion_at, json!(v)) }); }>
                                <option value="rrf" selected=fusion == "rrf">"Reciprocal rank (robust)"</option>
                                <option value="weighted" selected=fusion == "weighted">"Weighted scores"</option>
                            </select>
                        </div>
                        <Num label="RRF k" candidate at=base(&["fusion", "rrf_k"]) float=true />
                        <Num label="Results (top_k)" candidate at=base(&["top_k"]) />
                        <Num label="Extra hops" candidate at=base(&["multi_hop", "extra_hops"]) />
                    </div>
                </section>
                <section class="panel">
                    <h3>"Evidence quality"</h3>
                    <div class="row">
                        <Num label="Abstain below confidence" candidate at=base(&["abstention", "min_confidence"]) float=true />
                        <Num label="Minimum evidence" candidate at=base(&["abstention", "min_evidence"]) />
                        <Num label="Near-duplicate similarity" candidate at=base(&["dedup", "near_duplicate_similarity"]) float=true />
                        <Num label="Contradiction overlap" candidate at=base(&["contradiction", "min_overlap"]) float=true />
                    </div>
                </section>
            }.into_any()
        }}
        <ChangeFlow s=s.clone() candidate on_applied />
    }
}
