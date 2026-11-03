//! RAG2 administration console: overview, guided configuration (with preview, activation,
//! history and rollback), retrieval playground, documents, tenants and audit. Styled by the
//! Hesperides design system (`console/hesperides`); see `ui.rs`.

mod api;
mod copy;
mod pages;
mod ui;

use api::Session;
use leptos::prelude::*;
use serde_json::Value;
use ui::{Alert, Button, Card, Field, Icon, PageHead, TextInput, Tone, Variant};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Overview,
    Pipelines,
    Tenants,
    Config,
    Connectors,
    Playground,
    Documents,
    Audit,
}

impl Page {
    /// Sidebar label, page eyebrow (its group) and sprite icon.
    pub fn meta(self) -> (&'static str, &'static str, &'static str) {
        use copy::{group, nav};
        match self {
            Page::Overview => (nav::OVERVIEW, group::STATUS, "temple"),
            Page::Pipelines => (nav::PIPELINES, group::CONFIGURATION, "sliders"),
            Page::Tenants => (nav::TENANTS, group::CONFIGURATION, "user"),
            Page::Config => (nav::CONFIG, group::CONFIGURATION, "scroll"),
            Page::Connectors => (nav::CONNECTORS, group::CONFIGURATION, "link"),
            Page::Playground => (nav::PLAYGROUND, group::RETRIEVAL, "search"),
            Page::Documents => (nav::DOCUMENTS, group::RETRIEVAL, "list"),
            Page::Audit => (nav::AUDIT, group::GOVERNANCE, "quill"),
        }
    }

    pub fn eyebrow(self) -> &'static str {
        self.meta().1
    }
}

/// Sidebar sections in order.
const SECTIONS: [(&str, &[Page]); 4] = [
    (copy::group::STATUS, &[Page::Overview]),
    (
        copy::group::CONFIGURATION,
        &[
            Page::Pipelines,
            Page::Tenants,
            Page::Config,
            Page::Connectors,
        ],
    ),
    (copy::group::RETRIEVAL, &[Page::Playground, Page::Documents]),
    (copy::group::GOVERNANCE, &[Page::Audit]),
];

#[component]
fn Login(session: RwSignal<Option<Session>>) -> impl IntoView {
    use copy::login as t;
    let origin = web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .unwrap_or_default();
    let base = RwSignal::new(origin);
    let key = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let s = Session {
            base: base.get_untracked(),
            key: key.get_untracked(),
        };
        busy.set(true);
        leptos::task::spawn_local(async move {
            match s.get::<Value>("/v1/admin/config").await {
                Ok(_) => {
                    s.save();
                    session.set(Some(s));
                }
                Err(e) => error.set(Some(e)),
            }
            busy.set(false);
        });
    };
    view! {
        <main class="console-login">
            <Card>
                <form class="hs-stack" on:submit=submit>
                    <div class="console-brand"><img src=ui::LOGO_MARK alt="" /></div>
                    <PageHead eyebrow=t::EYEBROW title=t::TITLE />
                    <Field id="base" label=t::SERVER_URL><TextInput id="base" value=base /></Field>
                    <Field id="key" label=t::KEY hint=t::KEY_HINT><TextInput id="key" value=key secret=true /></Field>
                    <div class="hs-cluster">
                        <Button variant=Variant::Primary submit=true busy=busy icon="key">{t::SUBMIT}</Button>
                    </div>
                    {move || error.get().map(|e| view! {
                        <Alert tone=Tone::Danger title=copy::common::ERROR_TITLE>{e}</Alert>
                    })}
                </form>
            </Card>
        </main>
    }
}

#[component]
fn Shell(session: RwSignal<Option<Session>>) -> impl IntoView {
    let page = RwSignal::new(Page::Overview);
    let s = move || {
        session.get().unwrap_or(Session {
            base: String::new(),
            key: String::new(),
        })
    };
    let item = move |p: Page| {
        let (label, _, icon) = p.meta();
        view! {
            <li>
                <a href="#" class="hs-sidenav__item" class:is-active=move || page.get() == p
                    aria-current=move || (page.get() == p).then_some("page")
                    on:click=move |e| { e.prevent_default(); page.set(p); }>
                    <Icon name=icon />
                    <span class="hs-sidenav__label">{label}</span>
                </a>
            </li>
        }
    };
    view! {
        <div class="console-shell">
            <nav class="hs-sidenav" aria-label=copy::nav::LABEL>
                <div class="hs-sidenav__header">
                    <div class="console-brand">
                        <img src=ui::LOGO_MARK alt="" />
                        <p class="inscription">{copy::nav::BRAND}</p>
                    </div>
                </div>
                <div class="hs-sidenav__scroll">
                    {SECTIONS.iter().map(|(title, pages)| view! {
                        <div class="hs-sidenav__section">
                            <p class="hs-sidenav__title">{*title}</p>
                            <ul>{pages.iter().map(|p| item(*p)).collect_view()}</ul>
                        </div>
                    }).collect_view()}
                </div>
                <div class="hs-sidenav__footer">
                    <Button variant=Variant::Ghost icon="lock" on:click=move |_| { Session::clear(); session.set(None); }>
                        {copy::nav::SIGN_OUT}
                    </Button>
                </div>
            </nav>
            <main class="console-main hs-container hs-container--lg hs-container--padded">
                {move || match page.get() {
                    Page::Overview => view! { <pages::Overview s=s() /> }.into_any(),
                    Page::Pipelines => view! { <pages::Pipelines s=s() /> }.into_any(),
                    Page::Tenants => view! { <pages::Tenants s=s() /> }.into_any(),
                    Page::Config => view! { <pages::Config s=s() /> }.into_any(),
                    Page::Connectors => view! { <pages::Connectors s=s() /> }.into_any(),
                    Page::Playground => view! { <pages::Playground s=s() /> }.into_any(),
                    Page::Documents => view! { <pages::Documents s=s() /> }.into_any(),
                    Page::Audit => view! { <pages::Audit s=s() /> }.into_any(),
                }}
            </main>
        </div>
    }
}

#[component]
fn App() -> impl IntoView {
    let session = RwSignal::new(Session::load());
    view! {
        {move || if session.get().is_some() {
            view! { <Shell session /> }.into_any()
        } else {
            view! { <Login session /> }.into_any()
        }}
    }
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
