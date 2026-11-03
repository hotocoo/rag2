use super::{ErrorLine, text};
use crate::Page;
use crate::api::{Session, seg};
use crate::copy::connectors as t;
use crate::ui::{Alert, Badge, Button, Card, Empty, PageHead, Table, Tone, Variant};
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
                match s
                    .post::<_, Value>(
                        &format!("/v1/admin/connectors/{}/{what}", seg(&name)),
                        &json!({}),
                    )
                    .await
                {
                    Ok(v) if what == "test" => note.set(Some((
                        v["ok"].as_bool().unwrap_or(false),
                        text(&v["message"]),
                    ))),
                    Ok(v) => note.set(Some((true, t::report(&name, &v)))),
                    Err(e) => note.set(Some((false, e))),
                }
                reload();
            });
        }
    };
    view! {
        <PageHead eyebrow=Page::Connectors.eyebrow() title=t::TITLE lead=t::LEAD />
        <div class="hs-stack console-stack--section">
            <ErrorLine err />
            {move || note.get().map(|(ok, m)| view! {
                <Alert tone=if ok { Tone::Success } else { Tone::Danger }>{m}</Alert>
            })}
            <Card title=t::SOURCES>
                {move || if rows.with(Vec::is_empty) {
                    view! { <Empty icon="link" title=t::EMPTY_TITLE text=t::EMPTY_TEXT /> }.into_any()
                } else {
                    // Table children are FnOnce and take ownership; give them their own handle.
                    let act = act.clone();
                    view! {
                        <Table>
                            <thead><tr>
                                <th scope="col">{t::NAME}</th><th scope="col">{t::TENANT}</th><th scope="col">{t::KIND}</th>
                                <th scope="col">{t::EVERY}</th><th scope="col">{t::LAST_SYNC}</th><th scope="col"></th>
                            </tr></thead>
                            <tbody>
                                {rows.get().into_iter().map(|c| {
                                    let name = text(&c["name"]);
                                    let st = c["status"].clone();
                                    let last = match st["last_ok"].as_bool() {
                                        None => view! { <Badge tone=Tone::Neutral>{t::NEVER}</Badge> }.into_any(),
                                        Some(true) => view! { <Badge tone=Tone::Success>{t::fetched(&st["report"]["fetched"])}</Badge> }.into_any(),
                                        Some(false) => view! { <Badge tone=Tone::Danger>{st["message"].as_str().unwrap_or(t::FAILED).to_string()}</Badge> }.into_any(),
                                    };
                                    let (a1, a2) = (act.clone(), act.clone());
                                    let (n1, n2) = (name.clone(), name.clone());
                                    view! {
                                        <tr>
                                            <td><strong>{name}</strong></td>
                                            <td>{text(&c["tenant"])}</td>
                                            <td class="console-mono">{text(&c["kind"])}</td>
                                            <td class="hs-figures">{match c["interval_s"].as_u64() { Some(0) | None => t::ON_DEMAND.to_string(), Some(s) => t::interval(s) }}</td>
                                            <td>{last}</td>
                                            <td class="console-actions">
                                                <div class="hs-cluster">
                                                    <Button icon="refresh" on:click=move |_| a1(n1.clone(), "test")>{t::TEST}</Button>
                                                    <Button variant=Variant::Primary icon="download" on:click=move |_| a2(n2.clone(), "run")>{t::SYNC}</Button>
                                                </div>
                                            </td>
                                        </tr>
                                    }
                                }).collect_view()}
                            </tbody>
                        </Table>
                    }.into_any()
                }}
            </Card>
        </div>
    }
}
