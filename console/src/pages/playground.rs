use super::{ErrorLine, text};
use crate::Page;
use crate::api::{Session, seg};
use crate::copy::playground as t;
use crate::ui::{Alert, Button, Card, Field, PageHead, Stat, TextInput, Tone, Variant};
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
            match s
                .post::<_, Value>(
                    &path,
                    &json!({ "query": query.get_untracked(), "no_cache": true }),
                )
                .await
            {
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
        <PageHead eyebrow=Page::Playground.eyebrow() title=t::TITLE lead=t::LEAD />
        <div class="hs-stack console-stack--section">
            <Card>
                <form class="hs-cluster console-cluster--form" on:submit=run>
                    <Field id="pt" label=t::TENANT><TextInput id="pt" value=tenant /></Field>
                    <Field id="pq" label=t::QUERY class="console-grow"><TextInput id="pq" value=query /></Field>
                    <Button variant=Variant::Primary submit=true icon="search" busy=busy disabled=busy>{t::SEARCH}</Button>
                </form>
            </Card>
            <ErrorLine err />
            {move || result.get().map(|r| {
                let abst = r["abstained"].clone();
                // The summary card's children take `r`; keep the evidence list apart.
                let evidence = r["evidence"].as_array().cloned().unwrap_or_default();
                let route = t::route(
                    &r["route"]["retrievers"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" + ")).unwrap_or_default(),
                    r["route"]["reason"].as_str().unwrap_or_default(),
                );
                view! {
                    <Card eyebrow=t::RESULT>
                        <div class="hs-stack">
                            <div class="console-stats">
                                <Stat label=t::CONFIDENCE value=t::confidence(r["confidence"].as_f64().unwrap_or_default()) small=true />
                                <Stat label=t::ROUTE value=route small=true />
                                <Stat label=t::LATENCY value=t::micros(&r["timings"]["total_us"]) small=true />
                            </div>
                            {(!abst.is_null()).then(|| view! {
                                <Alert tone=Tone::Warning title=t::ABSTAINED>{t::abstained(&abst["reason"], abst["detail"].as_str().unwrap_or_default())}</Alert>
                            })}
                            {r["warnings"].as_array().cloned().unwrap_or_default().into_iter().map(|w| view! {
                                <Alert tone=Tone::Warning>{text(&w)}</Alert>
                            }).collect_view()}
                            {r["contradictions"].as_array().cloned().unwrap_or_default().into_iter().map(|c| view! {
                                <Alert tone=Tone::Danger title=t::CONTRADICTION>
                                    {t::contradiction(c["detector"].as_str().unwrap_or_default(), &c["a"], &c["b"], c["reason"].as_str().unwrap_or_default())}
                                </Alert>
                            }).collect_view()}
                        </div>
                    </Card>
                    {evidence.into_iter().enumerate().map(|(i, e)| {
                        let p = e["provenance"].clone();
                        let signals = e["signals"].as_array().cloned().unwrap_or_default().iter()
                            .map(|s| t::signal(s["retriever"].as_str().unwrap_or_default(), &s["rank"], s["score"].as_f64().unwrap_or_default()))
                            .collect::<Vec<_>>().join(", ");
                        view! {
                            <Card eyebrow=t::EVIDENCE title=text(&p["doc_id"]) outline=true>
                                <div class="hs-stack">
                                    <p class="console-muted body-sm hs-figures">
                                        {t::evidence_meta(i + 1, &p["chunk_ordinal"], &p["doc_version"], e["score"].as_f64().unwrap_or_default(), &signals)}
                                    </p>
                                    <p class="console-passage">{text(&e["text"])}</p>
                                </div>
                            </Card>
                        }
                    }).collect_view()}
                }
            })}
        </div>
    }
}
