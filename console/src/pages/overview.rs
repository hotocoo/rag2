use super::{ErrorLine, load, text};
use crate::Page;
use crate::api::Session;
use crate::copy::{common, overview as t};
use crate::ui::{Badge, Card, PageHead, Stat, Table, Tone};
use leptos::prelude::*;
use serde_json::Value;

#[component]
pub fn Overview(s: Session) -> impl IntoView {
    let cfg = RwSignal::new(None::<Value>);
    let tenants = RwSignal::new(None::<Value>);
    let comps = RwSignal::new(None::<Value>);
    let err = RwSignal::new(None::<String>);
    load(&s, "/v1/admin/config", cfg, err);
    load(&s, "/v1/admin/tenants", tenants, err);
    load(&s, "/v1/admin/components", comps, err);
    let or_pending = |v: Option<String>| v.unwrap_or_else(|| common::PENDING.to_string());
    view! {
        <PageHead eyebrow=Page::Overview.eyebrow() title=t::TITLE lead=t::LEAD />
        <div class="hs-stack console-stack--section">
            <ErrorLine err />
            <div class="console-stats">
                <Stat label=t::ACTIVE_VERSION value=Signal::derive(move || or_pending(cfg.get().map(|c| t::version(&c["version"])))) />
                <Stat label=t::PROFILE value=Signal::derive(move || or_pending(cfg.get().map(|c| text(&c["name"])))) />
                <Stat label=t::TENANTS value=Signal::derive(move || or_pending(tenants.get().and_then(|t| t.as_array().map(|a| a.len().to_string())))) />
                <Stat label=t::ROLE value=Signal::derive(move || or_pending(cfg.get().map(|c| c["profile"]["cluster"]["role"].as_str().unwrap_or(t::DEFAULT_ROLE).to_string()))) />
            </div>
            <Card title=t::COMPONENTS>
                {move || comps.get().map(|c| view! {
                    <Table>
                        <thead><tr><th scope="col">{t::KIND}</th><th scope="col">{t::NAME}</th><th scope="col">{t::STATUS}</th></tr></thead>
                        <tbody>
                            {c["embedders"].as_array().cloned().unwrap_or_default().into_iter().map(|e| view! {
                                <tr><td>{t::EMBEDDER}</td><td class="console-mono">{text(&e)}</td><td><Badge tone=Tone::Success>{t::LOADED}</Badge></td></tr>
                            }).collect_view()}
                            {c["decision_models"].as_array().cloned().unwrap_or_default().into_iter().map(|d| {
                                let healthy = d[1].as_bool().unwrap_or(false);
                                view! {
                                    <tr>
                                        <td>{t::DECISION_MODEL}</td>
                                        <td class="console-mono">{text(&d[0])}</td>
                                        <td>{if healthy {
                                            view! { <Badge tone=Tone::Success>{t::HEALTHY}</Badge> }
                                        } else {
                                            view! { <Badge tone=Tone::Danger>{t::UNREACHABLE}</Badge> }
                                        }}</td>
                                    </tr>
                                }
                            }).collect_view()}
                        </tbody>
                    </Table>
                })}
            </Card>
        </div>
    }
}
