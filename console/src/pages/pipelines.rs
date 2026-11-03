use super::{ChangeFlow, ErrorLine, SelectField, text};
use crate::Page;
use crate::api::Session;
use crate::copy::pipelines as t;
use crate::ui::{Card, Checkbox, Field, InputFrame, Loader, PageHead, Table};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Value, json};

/// Input step for fractional settings (weights, thresholds).
const FLOAT_STEP: &str = "0.01";
/// Input step for counts.
const INT_STEP: &str = "1";
/// Fusion methods the server accepts, with their labels.
const FUSION_METHODS: [(&str, &str); 2] =
    [("rrf", t::FUSION_RRF), ("weighted", t::FUSION_WEIGHTED)];
/// Fusion method when the profile names none.
const DEFAULT_FUSION: &str = "rrf";
/// Pipeline selected when the profile names no default.
const DEFAULT_PIPELINE: &str = "default";

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
    let field_id = id.clone();
    let input = view! {
        <InputFrame>
            <input id=id class="hs-input__control hs-figures" aria-label=label type="number"
                step=if float { FLOAT_STEP } else { INT_STEP }
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
        </InputFrame>
    };
    if compact {
        input.into_any()
    } else {
        view! { <Field id=field_id label=label>{input}</Field> }.into_any()
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
                            selected.set(
                                p["default_pipeline"]
                                    .as_str()
                                    .unwrap_or(DEFAULT_PIPELINE)
                                    .to_string(),
                            );
                        }
                        candidate.set(Some(p));
                    }
                    Err(e) => err.set(Some(e)),
                }
            });
        }
    };
    reload();
    let names = Signal::derive(move || {
        candidate.with(|c| {
            c.as_ref()
                .and_then(|c| {
                    c["pipelines"]
                        .as_object()
                        .map(|o| o.keys().map(|k| (k.clone(), k.clone())).collect::<Vec<_>>())
                })
                .unwrap_or_default()
        })
    });
    let on_applied = Callback::new(move |_| reload());
    view! {
        <PageHead eyebrow=Page::Pipelines.eyebrow() title=t::TITLE lead=t::LEAD />
        <div class="hs-stack console-stack--section">
            <ErrorLine err />
            <Card>
                <SelectField id="pipeline" label=t::PIPELINE options=names selected=selected
                    on_change=Callback::new(move |v| selected.set(v)) />
            </Card>
            {move || {
                let name = selected.get();
                if name.is_empty() || candidate.with(|c| c.is_none()) {
                    return view! { <p class="console-muted"><Loader /></p> }.into_any();
                }
                let base = |rest: &[&str]| { let mut v = path(&["pipelines", name.as_str()]); v.extend(path(rest)); v };
                let retrievers = candidate.with(|c| c.as_ref().map(|c| c["pipelines"][name.as_str()]["retrievers"].as_array().cloned().unwrap_or_default()).unwrap_or_default());
                let fusion = candidate.with(|c| c.as_ref().map(|c| c["pipelines"][name.as_str()]["fusion"]["method"].as_str().unwrap_or(DEFAULT_FUSION).to_string()).unwrap_or_default());
                let fusion_at = base(&["fusion", "method"]);
                let fusion_options = FUSION_METHODS.iter().map(|(v, l)| (v.to_string(), l.to_string())).collect::<Vec<_>>();
                // Card children are FnOnce and must own what they render: build the views here.
                let retriever_rows = retrievers.into_iter().enumerate().map(|(i, r)| {
                    let idx = i.to_string();
                    let enabled_at = base(&["retrievers", idx.as_str(), "enabled"]);
                    let on = r["enabled"].as_bool().unwrap_or(true);
                    view! {
                        <tr>
                            <td class="console-mono">{t::retriever(&text(&r["kind"]), r["embedder"].as_str())}</td>
                            <td>
                                <Checkbox label=t::ENABLED checked=on
                                    on:change=move |e| { let v = event_target_checked(&e); candidate.update(|c| if let Some(c) = c { set_at(c, &enabled_at, json!(v)) }); } />
                            </td>
                            <td><Num label=t::WEIGHT compact=true candidate at=base(&["retrievers", idx.as_str(), "weight"]) float=true /></td>
                            <td><Num label=t::CANDIDATES compact=true candidate at=base(&["retrievers", idx.as_str(), "top_k"]) /></td>
                        </tr>
                    }
                }).collect_view();
                let ranking = view! {
                    <SelectField id="fusion" label=t::FUSION options=fusion_options selected=fusion
                        on_change=Callback::new(move |v: String| candidate.update(|c| if let Some(c) = c { set_at(c, &fusion_at, json!(v)) })) />
                    <Num label=t::RRF_K candidate at=base(&["fusion", "rrf_k"]) float=true />
                    <Num label=t::RESULTS candidate at=base(&["top_k"]) />
                    <Num label=t::EXTRA_HOPS candidate at=base(&["multi_hop", "extra_hops"]) />
                };
                let quality = view! {
                    <Num label=t::MIN_CONFIDENCE candidate at=base(&["abstention", "min_confidence"]) float=true />
                    <Num label=t::MIN_EVIDENCE candidate at=base(&["abstention", "min_evidence"]) />
                    <Num label=t::NEAR_DUPLICATE candidate at=base(&["dedup", "near_duplicate_similarity"]) float=true />
                    <Num label=t::CONTRADICTION candidate at=base(&["contradiction", "min_overlap"]) float=true />
                };
                view! {
                    <Card title=t::RETRIEVERS>
                        <Table>
                            <thead><tr><th scope="col">{t::KIND}</th><th scope="col">{t::ENABLED}</th><th scope="col">{t::WEIGHT}</th><th scope="col">{t::CANDIDATES}</th></tr></thead>
                            <tbody>{retriever_rows}</tbody>
                        </Table>
                    </Card>
                    <Card title=t::RANKING>
                        <div class="hs-cluster console-cluster--form">{ranking}</div>
                    </Card>
                    <Card title=t::QUALITY>
                        <div class="hs-cluster console-cluster--form">{quality}</div>
                    </Card>
                }.into_any()
            }}
            <ChangeFlow s=s.clone() candidate on_applied />
        </div>
    }
}
