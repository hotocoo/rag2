use super::{ErrorLine, load, text};
use crate::Page;
use crate::api::{Session, to_yaml};
use crate::copy::config as t;
use crate::ui::{
    Alert, Badge, Button, Card, CodeBlock, Field, Frame, InputFrame, PageHead, Table, TextInput,
    Tone, Variant,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{Value, json};

/// Rows the YAML editor opens with.
const EDITOR_ROWS: u32 = 24;

/// The whole profile as YAML (for anything the guided pages do not cover), plus version history
/// with activation and rollback.
#[component]
pub fn Config(s: Session) -> impl IntoView {
    let text_value = RwSignal::new(String::new());
    let versions = RwSignal::new(None::<Value>);
    let preview = RwSignal::new(None::<Value>);
    let comment = RwSignal::new(String::new());
    let err = RwSignal::new(None::<String>);
    let info = RwSignal::new(None::<&'static str>);
    let reload = {
        let s = s.clone();
        move || {
            let s = s.clone();
            load(&s, "/v1/admin/config/versions", versions, err);
            spawn_local(async move {
                match s.get::<Value>("/v1/admin/config").await {
                    Ok(v) => text_value.set(to_yaml(&v["profile"])),
                    Err(e) => err.set(Some(e)),
                }
            });
        }
    };
    reload();
    let call = {
        let s = s.clone();
        let reload = reload.clone();
        move |path: String, body: Value, done: Option<&'static str>| {
            let (s, reload) = (s.clone(), reload.clone());
            spawn_local(async move {
                match s.post::<_, Value>(&path, &body).await {
                    Ok(v) => {
                        if path.ends_with("/preview") {
                            preview.set(Some(v));
                        } else {
                            preview.set(None);
                            info.set(done);
                            reload();
                        }
                        err.set(None);
                    }
                    Err(e) => err.set(Some(e)),
                }
            });
        }
    };
    let (c1, c2, c3, c4) = (call.clone(), call.clone(), call.clone(), call);
    view! {
        <PageHead eyebrow=Page::Config.eyebrow() title=t::TITLE lead=t::LEAD />
        <div class="hs-stack console-stack--section">
            <ErrorLine err />
            {move || info.get().map(|i| view! { <Alert tone=Tone::Success>{i}</Alert> })}
            <Card title=t::EDITOR>
                <div class="hs-stack">
                    <Field id="yaml" label=t::YAML hint=t::YAML_HINT>
                        <InputFrame frame=Frame::Area>
                            <textarea id="yaml" class="hs-input__control console-mono" spellcheck="false" rows=EDITOR_ROWS
                                prop:value=move || text_value.get() on:input=move |e| text_value.set(event_target_value(&e))></textarea>
                        </InputFrame>
                    </Field>
                    <div class="hs-cluster console-cluster--form">
                        <Field id="note" label=t::NOTE class="console-grow"><TextInput id="note" value=comment /></Field>
                        <Button icon="eye" on:click=move |_| c1("/v1/admin/config/preview".into(), json!({"text": text_value.get_untracked()}), None)>{t::VALIDATE}</Button>
                        <Button variant=Variant::Primary icon="check"
                            on:click=move |_| c2("/v1/admin/config/versions".into(), json!({"text": text_value.get_untracked(), "comment": comment.get_untracked(), "activate": true}), Some(t::SAVED))>
                            {t::SAVE}
                        </Button>
                    </div>
                    {move || preview.get().map(|p| {
                        let valid = p["valid"].as_bool().unwrap_or(false);
                        let diff = serde_json::to_string_pretty(&json!({"changes": p["changes"], "issues": p["issues"]})).unwrap_or_default();
                        view! {
                            {if valid {
                                view! { <Alert tone=Tone::Success>{t::VALID}" "{crate::copy::change::count(p["changes"].as_array().map_or(0, Vec::len))}</Alert> }
                            } else {
                                view! { <Alert tone=Tone::Danger>{t::INVALID}" "{crate::copy::change::count(p["changes"].as_array().map_or(0, Vec::len))}</Alert> }
                            }}
                            <CodeBlock title=t::DIFF code=diff />
                        }
                    })}
                </div>
            </Card>
            <Card title=t::HISTORY>
                <div class="hs-stack">
                    <div class="hs-cluster">
                        <Button icon="refresh" on:click=move |_| c3("/v1/admin/config/rollback".into(), json!({}), Some(t::ROLLED_BACK))>{t::ROLLBACK}</Button>
                    </div>
                    {move || versions.get().map(|v| {
                        let active = v["active"].as_u64().unwrap_or(0);
                        let mut list = v["versions"].as_array().cloned().unwrap_or_default();
                        list.reverse();
                        let act = c4.clone();
                        view! {
                            <Table>
                                <thead><tr><th scope="col">{t::VERSION}</th><th scope="col">{t::AUTHOR}</th><th scope="col">{t::COMMENT}</th><th scope="col"></th></tr></thead>
                                <tbody>
                                    {list.into_iter().map(|m| {
                                        let n = m["version"].as_u64().unwrap_or(0);
                                        let act = act.clone();
                                        view! {
                                            <tr>
                                                <td>
                                                    <div class="hs-cluster">
                                                        <span class="hs-figures">{t::version(n)}</span>
                                                        {(n == active).then(|| view! { <Badge tone=Tone::Success>{t::ACTIVE}</Badge> })}
                                                    </div>
                                                </td>
                                                <td>{text(&m["author"])}</td>
                                                <td>{text(&m["comment"])}</td>
                                                <td>{(n != active).then(|| view! {
                                                    <Button icon="arrow-up" on:click=move |_| act(format!("/v1/admin/config/versions/{n}/activate"), json!({}), Some(t::ACTIVATED))>{t::ACTIVATE}</Button>
                                                })}</td>
                                            </tr>
                                        }
                                    }).collect_view()}
                                </tbody>
                            </Table>
                        }
                    })}
                </div>
            </Card>
        </div>
    }
}
