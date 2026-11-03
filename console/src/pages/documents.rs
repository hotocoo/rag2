use super::{ErrorLine, text};
use crate::Page;
use crate::api::{Session, seg};
use crate::copy::documents as t;
use crate::ui::{Button, Card, Empty, Field, PageHead, Table, TextInput, Variant};
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
            let path = format!(
                "/v1/tenants/{}/documents/{}/chunks",
                seg(&tenant.get_untracked()),
                seg(&id)
            );
            match s.get::<Vec<Value>>(&path).await {
                Ok(c) => chunks.set(Some((id, c))),
                Err(e) => err.set(Some(e)),
            }
        });
    };
    view! {
        <PageHead eyebrow=Page::Documents.eyebrow() title=t::TITLE lead=t::LEAD />
        <div class="hs-stack console-stack--section">
            <Card>
                <form class="hs-cluster console-cluster--form" on:submit=move |ev| { ev.prevent_default(); l1(None); }>
                    <Field id="dt" label=t::TENANT class="console-grow"><TextInput id="dt" value=tenant /></Field>
                    <Button variant=Variant::Primary submit=true icon="list">{t::LIST}</Button>
                </form>
            </Card>
            <ErrorLine err />
            <Card title=t::TITLE>
                <div class="hs-stack">
                    {move || stats.get().map(|st| view! { <p class="console-muted body-sm hs-figures">{t::stats(&st)}</p> })}
                    {move || if docs.with(Vec::is_empty) {
                        view! { <Empty icon="scroll" title=t::EMPTY_TITLE text=t::EMPTY_TEXT /> }.into_any()
                    } else {
                        // Table children are FnOnce and take ownership; give them their own handle.
                        let show = show.clone();
                        view! {
                            <Table>
                                <thead><tr>
                                    <th scope="col">{t::ID}</th><th scope="col">{t::DOC_TITLE}</th><th scope="col">{t::VERSION}</th>
                                    <th scope="col">{t::CHUNKS}</th><th scope="col"></th>
                                </tr></thead>
                                <tbody>
                                    {docs.get().into_iter().map(|d| {
                                        let id = text(&d["id"]);
                                        let sh = show.clone();
                                        let idc = id.clone();
                                        view! {
                                            <tr>
                                                <td class="console-mono">{id}</td>
                                                <td>{text(&d["title"])}</td>
                                                <td class="is-numeric hs-figures">{d["version"].to_string()}</td>
                                                <td class="is-numeric hs-figures">{d["chunks"].as_array().map_or(0, Vec::len)}</td>
                                                <td><Button icon="eye" on:click=move |_| sh(idc.clone())>{t::CHUNKS}</Button></td>
                                            </tr>
                                        }
                                    }).collect_view()}
                                </tbody>
                            </Table>
                        }.into_any()
                    }}
                    <div class="hs-cluster">
                        <Button icon="arrow-right" disabled=Signal::derive(move || docs.with(|d| d.len() < PAGE))
                            on:click=move |_| { let last = docs.with(|d| d.last().and_then(|x| x["id"].as_str().map(String::from))); l2(last); }>
                            {t::NEXT}
                        </Button>
                    </div>
                </div>
            </Card>
            {move || chunks.get().map(|(id, cs)| view! {
                <Card title=t::chunks_of(&id)>
                    <div class="hs-stack hs-stack--divided">
                        {cs.into_iter().map(|c| view! {
                            <article class="hs-stack">
                                <p class="console-muted body-sm hs-figures">{t::chunk_meta(&c["ordinal"], &c["id"])}</p>
                                <p class="console-passage">{text(&c["text"])}</p>
                            </article>
                        }).collect_view()}
                    </div>
                </Card>
            })}
        </div>
    }
}
