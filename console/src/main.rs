//! RAG2 administration console: overview, guided configuration (with preview, activation,
//! history and rollback), retrieval playground, documents, tenants and audit.

mod api;
mod pages;

use api::Session;
use leptos::prelude::*;

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

const PAGES: [(Page, &str); 8] = [
    (Page::Overview, "Overview"),
    (Page::Pipelines, "Pipelines"),
    (Page::Tenants, "Tenants & quotas"),
    (Page::Config, "Profile & versions"),
    (Page::Connectors, "Connectors"),
    (Page::Playground, "Playground"),
    (Page::Documents, "Documents"),
    (Page::Audit, "Audit log"),
];

#[component]
fn Login(session: RwSignal<Option<Session>>) -> impl IntoView {
    let origin = web_sys::window().and_then(|w| w.location().origin().ok()).unwrap_or_default();
    let base = RwSignal::new(origin);
    let key = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let s = Session { base: base.get_untracked(), key: key.get_untracked() };
        leptos::task::spawn_local(async move {
            match s.get::<Value>("/v1/admin/config").await {
                Ok(_) => {
                    s.save();
                    session.set(Some(s));
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };
    view! {
        <main style="max-width:420px;margin:10vh auto">
            <h2>"RAG2 console"</h2>
            <form class="panel" on:submit=submit>
                <label for="base">"Server URL"</label>
                <input id="base" prop:value=move || base.get() on:input=move |e| base.set(event_target_value(&e)) />
                <label for="key" style="margin-top:12px">"API key or access token"</label>
                <input id="key" type="password" autocomplete="off" prop:value=move || key.get()
                    on:input=move |e| key.set(event_target_value(&e)) />
                <p class="muted">"Needs the manage_config permission. Kept in this tab's session storage only."</p>
                <button class="primary" type="submit">"Sign in"</button>
                {move || error.get().map(|e| view! { <p class="err" role="alert">{e}</p> })}
            </form>
        </main>
    }
}

use serde_json::Value;

#[component]
fn Shell(session: RwSignal<Option<Session>>) -> impl IntoView {
    let page = RwSignal::new(Page::Overview);
    let s = move || session.get().unwrap_or(Session { base: String::new(), key: String::new() });
    view! {
        <div class="shell">
            <nav class="side" aria-label="Console sections">
                <h1>"RAG2"</h1>
                {PAGES
                    .iter()
                    .map(|(p, label)| {
                        let p = *p;
                        view! {
                            <button class:active=move || page.get() == p
                                aria-current=move || if page.get() == p { "page" } else { "false" }
                                on:click=move |_| page.set(p)>{*label}</button>
                        }
                    })
                    .collect_view()}
                <button on:click=move |_| { Session::clear(); session.set(None); }>"Sign out"</button>
            </nav>
            <main>
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
