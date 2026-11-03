use super::pipelines::set_at;
use super::{ChangeFlow, ErrorLine};
use crate::Page;
use crate::api::{Session, seg};
use crate::copy::{common, tenants as t};
use crate::ui::{
    Badge, Button, Card, Empty, Field, InputFrame, PageHead, Table, TextInput, Tone, Variant,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Value, json};

/// Defaults written for a newly added tenant override; the server validates them.
const NEW_TENANT: &str =
    r#"{"requests_per_second": 100, "burst": 200, "max_documents": 1000000, "enabled": true}"#;

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
        let configured: Vec<String> = candidate.with(|c| {
            c.as_ref()
                .and_then(|c| {
                    c["tenants"]
                        .as_object()
                        .map(|o| o.keys().cloned().collect())
                })
                .unwrap_or_default()
        });
        let mut all: Vec<String> = configured.into_iter().chain(live.get()).collect();
        all.sort();
        all.dedup();
        all
    };
    let field = move |tenant: String, f: &'static str, label: &'static str| {
        let at = vec!["tenants".to_string(), tenant.clone(), f.to_string()];
        let shown = move || {
            candidate.with(|c| {
                c.as_ref()
                    .map(|c| {
                        let v = &c["tenants"][tenant.as_str()][f];
                        if v.is_null() {
                            c["tenant_defaults"][f].to_string()
                        } else {
                            v.to_string()
                        }
                    })
                    .unwrap_or_default()
            })
        };
        view! {
            <InputFrame>
                <input type="number" class="hs-input__control hs-figures" aria-label=label prop:value=shown on:change=move |e| {
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
            </InputFrame>
        }
    };
    let s_del = s.clone();
    let reload2 = reload.clone();
    let delete = move |tenant: String| {
        let s = s_del.clone();
        let reload = reload2.clone();
        if confirm.get_untracked() != tenant {
            err.set(Some(t::confirm_needed(&tenant)));
            return;
        }
        spawn_local(async move {
            match s
                .delete(&format!(
                    "/v1/admin/tenants/{}?confirm={}",
                    seg(&tenant),
                    seg(&tenant)
                ))
                .await
            {
                Ok(_) => {
                    confirm.set(String::new());
                    reload();
                }
                Err(e) => err.set(Some(e)),
            }
        });
    };
    let add = move |_| {
        let id = new_id.get_untracked().trim().to_string();
        if id.is_empty() {
            return;
        }
        candidate.update(|c| {
            if let Some(c) = c {
                set_at(
                    c,
                    &["tenants".into(), id.clone()],
                    serde_json::from_str(NEW_TENANT).unwrap_or(json!({})),
                );
            }
        });
        new_id.set(String::new());
    };
    let on_applied = Callback::new(move |_| reload());
    view! {
        <PageHead eyebrow=Page::Tenants.eyebrow() title=t::TITLE lead=t::LEAD />
        <div class="hs-stack console-stack--section">
            <ErrorLine err />
            <Card title=t::QUOTAS>
                <div class="hs-stack">
                    {move || if rows().is_empty() {
                        view! { <Empty icon="user" title=t::EMPTY_TITLE text=t::EMPTY_TEXT /> }.into_any()
                    } else {
                        // Table children are FnOnce and take ownership; give them their own handle.
                        let delete = delete.clone();
                        view! {
                            <Table>
                                <thead><tr>
                                    <th scope="col">{t::TENANT}</th><th scope="col">{t::RPS}</th><th scope="col">{t::BURST}</th>
                                    <th scope="col">{t::MAX_DOCS}</th><th scope="col">{t::HAS_DATA}</th><th scope="col"></th>
                                </tr></thead>
                                <tbody>
                                    {rows().into_iter().map(|tenant| {
                                        let has = live.get().contains(&tenant);
                                        let del = delete.clone();
                                        let tid = tenant.clone();
                                        view! {
                                            <tr>
                                                <td><strong>{tenant.clone()}</strong></td>
                                                <td>{field(tenant.clone(), "requests_per_second", t::RPS)}</td>
                                                <td>{field(tenant.clone(), "burst", t::BURST)}</td>
                                                <td>{field(tenant.clone(), "max_documents", t::MAX_DOCS)}</td>
                                                <td>{if has {
                                                    view! { <Badge tone=Tone::Info>{common::YES}</Badge> }
                                                } else {
                                                    view! { <Badge tone=Tone::Neutral>{common::NO}</Badge> }
                                                }}</td>
                                                <td>{has.then(|| view! {
                                                    <Button variant=Variant::Danger icon="trash" on:click=move |_| del(tid.clone())>{t::DELETE}</Button>
                                                })}</td>
                                            </tr>
                                        }
                                    }).collect_view()}
                                </tbody>
                            </Table>
                        }.into_any()
                    }}
                    <div class="hs-cluster console-cluster--form">
                        <Field id="newt" label=t::ADD_LABEL><TextInput id="newt" value=new_id placeholder=t::ADD_PLACEHOLDER /></Field>
                        <Button icon="plus" on:click=add>{t::ADD}</Button>
                        <Field id="confirm" label=t::CONFIRM_LABEL hint=t::CONFIRM_HINT><TextInput id="confirm" value=confirm /></Field>
                    </div>
                </div>
            </Card>
            <ChangeFlow s=s.clone() candidate on_applied />
        </div>
    }
}
