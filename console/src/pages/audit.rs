use super::{ErrorLine, text};
use crate::Page;
use crate::api::Session;
use crate::copy::audit as t;
use crate::ui::{Badge, Button, Card, Empty, PageHead, Table, Tone};
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
                    events
                        .update(|e| e.extend(v["events"].as_array().cloned().unwrap_or_default()));
                    next.set(v["next"].as_str().map(String::from));
                }
                Err(e) => err.set(Some(e)),
            }
        });
    };
    fetch(None);
    view! {
        <PageHead eyebrow=Page::Audit.eyebrow() title=t::TITLE lead=t::LEAD />
        <div class="hs-stack console-stack--section">
            <ErrorLine err />
            <Card title=t::EVENTS>
                <div class="hs-stack">
                    {move || if events.with(Vec::is_empty) {
                        view! { <Empty icon="scroll" title=t::EMPTY_TITLE text=t::EMPTY_TEXT /> }.into_any()
                    } else {
                        view! {
                            <Table>
                                <thead><tr>
                                    <th scope="col">{t::TIME}</th><th scope="col">{t::PRINCIPAL}</th><th scope="col">{t::ACTION}</th>
                                    <th scope="col">{t::TENANT}</th><th scope="col">{t::TARGET}</th><th scope="col">{t::OUTCOME}</th>
                                </tr></thead>
                                <tbody>
                                    {events.get().into_iter().map(|e| {
                                        let tone = if e["outcome"] == "ok" { Tone::Success } else { Tone::Warning };
                                        view! {
                                            <tr>
                                                <td class="is-numeric hs-figures">{e["at_ms"].to_string()}</td>
                                                <td>{text(&e["principal"])}</td>
                                                <td class="console-mono">{text(&e["action"])}</td>
                                                <td>{text(&e["tenant"])}</td>
                                                <td>{text(&e["target"])}</td>
                                                <td><Badge tone>{text(&e["outcome"])}</Badge></td>
                                            </tr>
                                        }
                                    }).collect_view()}
                                </tbody>
                            </Table>
                        }.into_any()
                    }}
                    <div class="hs-cluster">
                        <Button icon="chevron-down" disabled=Signal::derive(move || next.get().is_none())
                            on:click=move |_| fetch(next.get_untracked())>{t::MORE}</Button>
                    </div>
                </div>
            </Card>
        </div>
    }
}
