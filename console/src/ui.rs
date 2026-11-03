//! Hesperides components for Leptos. Each one renders the DOM and `hs-*` classes that the
//! upstream React component renders (Hesperides `components/bundle.js`), so the copied
//! `bundle.css` styles it unchanged. Behaviour that needs `bundle.js` (press gleam, sliding nav
//! indicator, copy-to-clipboard) is left out.

use leptos::prelude::*;

/// Icon sprite and logo mark, copied next to `index.html` by trunk (see `index.html`).
pub const ICON_SPRITE: &str = "hesperides-icons.svg";
pub const LOGO_MARK: &str = "hesperides-mark-gilt.svg";

/// Status and emphasis tones shared by Alert and Badge.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Success,
    Warning,
    Danger,
    Neutral,
}

impl Tone {
    fn class(self) -> &'static str {
        match self {
            Tone::Info => "info",
            Tone::Success => "success",
            Tone::Warning => "warning",
            Tone::Danger => "danger",
            Tone::Neutral => "neutral",
        }
    }

    /// Status always carries its icon, never colour alone.
    fn icon(self) -> &'static str {
        match self {
            Tone::Info => "info",
            Tone::Success => "success",
            Tone::Warning => "warning",
            Tone::Danger => "error",
            Tone::Neutral => "astra",
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum IconSize {
    Xs,
    Sm,
    #[default]
    Md,
    Lg,
}

/// A sprite icon in `currentColor`, hidden from assistive tech (pair it with text).
#[component]
pub fn Icon(name: &'static str, #[prop(optional)] size: IconSize) -> impl IntoView {
    let class = match size {
        IconSize::Xs => "hs-icon console-icon--xs",
        IconSize::Sm => "hs-icon console-icon--sm",
        IconSize::Md => "hs-icon",
        IconSize::Lg => "hs-icon console-icon--lg",
    };
    // `name` is always a compile-time icon id, never user input.
    let sprite_ref = format!("<use href=\"{ICON_SPRITE}#i-{name}\"></use>");
    view! {
        <svg class=class viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-linecap="square"
            stroke-linejoin="miter" aria-hidden="true" focusable="false" inner_html=sprite_ref></svg>
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Variant {
    Primary,
    #[default]
    Secondary,
    Ghost,
    Danger,
}

impl Variant {
    fn class(self) -> &'static str {
        match self {
            Variant::Primary => "hs-btn hs-btn--primary hs-btn--md",
            Variant::Secondary => "hs-btn hs-btn--secondary hs-btn--md",
            Variant::Ghost => "hs-btn hs-btn--ghost hs-btn--md",
            Variant::Danger => "hs-btn hs-btn--danger hs-btn--md",
        }
    }
}

/// Button. Add `on:click` at the call site; it lands on the `<button>`.
#[component]
pub fn Button(
    #[prop(optional)] variant: Variant,
    #[prop(optional)] submit: bool,
    #[prop(optional)] icon: Option<&'static str>,
    #[prop(into, optional)] disabled: Signal<bool>,
    #[prop(into, optional)] busy: Signal<bool>,
    children: Children,
) -> impl IntoView {
    view! {
        <button class=variant.class() class:is-loading=move || busy.get()
            type=if submit { "submit" } else { "button" }
            disabled=move || disabled.get() aria-busy=move || busy.get().then_some("true")>
            <span class="hs-btn__label">
                {icon.map(|name| view! { <Icon name size=IconSize::Md /> })}
                <span class="hs-btn__text">{children()}</span>
            </span>
            {move || busy.get().then(|| view! { <span class="hs-btn__layer"><Loader /></span> })}
        </button>
    }
}

/// Small arc loader with a spoken label.
#[component]
pub fn Loader() -> impl IntoView {
    view! {
        <span role="status" aria-live="polite" class="hs-loader hs-allow-motion hs-loader--arc">
            <svg viewBox="0 0 24 24" class="hs-loader__arc hs-icon console-icon--sm" aria-hidden="true">
                <circle cx="12" cy="12" r="9" class="hs-loader__track"></circle>
                <path d="M12 3a9 9 0 0 1 9 9" class="hs-loader__head"></path>
            </svg>
            <span class="hs-sr">{crate::copy::common::LOADING}</span>
        </span>
    }
}

/// Label and hint around one control; `id` must match the control's id.
#[component]
pub fn Field(
    #[prop(into)] id: String,
    label: &'static str,
    #[prop(optional)] hint: Option<&'static str>,
    #[prop(optional)] class: &'static str,
    children: Children,
) -> impl IntoView {
    view! {
        <div class=format!("hs-field {class}")>
            <div class="hs-field__head"><label for=id class="hs-field__label">{label}</label></div>
            {children()}
            {hint.map(|h| view! { <p class="hs-field__hint">{h}</p> })}
        </div>
    }
}

/// Which `hs-input` frame a control sits in.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Frame {
    #[default]
    Text,
    Select,
    Area,
}

/// The `hs-input` frame. Give the control inside it `class="hs-input__control"`.
#[component]
pub fn InputFrame(#[prop(optional)] frame: Frame, children: Children) -> impl IntoView {
    let class = match frame {
        Frame::Text => "hs-input hs-input--md hs-input--boxed",
        Frame::Select => "hs-input hs-input--select hs-input--md",
        Frame::Area => "hs-input hs-input--area",
    };
    view! {
        <div class=class>
            {children()}
            {(frame == Frame::Select).then(|| view! {
                <span class="hs-input__chevron"><Icon name="chevron-down" size=IconSize::Sm /></span>
            })}
        </div>
    }
}

/// A text input bound to a signal.
#[component]
pub fn TextInput(
    #[prop(into)] id: String,
    value: RwSignal<String>,
    #[prop(optional)] secret: bool,
    #[prop(optional)] placeholder: &'static str,
) -> impl IntoView {
    view! {
        <InputFrame>
            <input id=id class="hs-input__control" type=if secret { "password" } else { "text" }
                autocomplete=secret.then_some("off") placeholder=placeholder
                prop:value=move || value.get() on:input=move |e| value.set(event_target_value(&e)) />
        </InputFrame>
    }
}

/// Tick and dash drawn inside the checkbox box (Hesperides Checkbox geometry).
const CHECK_TICK: &str = "M3.5 8.5l3 3 6-7";
const CHECK_DASH: &str = "M4 8h8";

/// A small checkbox. Add `on:change` at the call site; the input's change event bubbles to it.
#[component]
pub fn Checkbox(label: &'static str, checked: bool) -> impl IntoView {
    view! {
        <label class="hs-check hs-check--sm">
            <input type="checkbox" class="hs-check__input" aria-label=label prop:checked=checked />
            <span class="hs-check__box" aria-hidden="true">
                <svg viewBox="0 0 16 16">
                    <path class="hs-check__tick" d=CHECK_TICK></path>
                    <path class="hs-check__dash" d=CHECK_DASH></path>
                </svg>
            </span>
        </label>
    }
}

/// A surface card with an optional eyebrow and title.
#[component]
pub fn Card(
    #[prop(optional)] eyebrow: Option<&'static str>,
    #[prop(optional, into)] title: Option<String>,
    #[prop(optional, into)] label: Option<String>,
    #[prop(optional)] outline: bool,
    children: Children,
) -> impl IntoView {
    let class = if outline {
        "hs-card hs-card--outline hs-card--pad-md"
    } else {
        "hs-card hs-card--surface hs-card--pad-md"
    };
    view! {
        <section class=class aria-label=label>
            <div class="hs-card__body">
                {eyebrow.map(|e| view! { <p class="hs-eyebrow">{e}</p> })}
                {title.map(|t| view! { <h3 class="hs-card__title heading-4">{t}</h3> })}
                <div class="hs-card__content">{children()}</div>
            </div>
        </section>
    }
}

/// An inline message with its status icon. Danger and warning are announced assertively.
#[component]
pub fn Alert(
    tone: Tone,
    #[prop(optional)] title: Option<&'static str>,
    children: Children,
) -> impl IntoView {
    let role = if matches!(tone, Tone::Danger | Tone::Warning) {
        "alert"
    } else {
        "status"
    };
    view! {
        <div role=role class=format!("hs-alert hs-alert--{} hs-alert--soft", tone.class())>
            <div class="hs-alert__inner">
                <span class="hs-alert__icon"><Icon name=tone.icon() /></span>
                <div class="hs-alert__body">
                    {title.map(|t| view! { <p class="hs-alert__title">{t}</p> })}
                    <div class="hs-alert__text">{children()}</div>
                </div>
            </div>
        </div>
    }
}

/// A status word with its icon.
#[component]
pub fn Badge(tone: Tone, children: Children) -> impl IntoView {
    view! {
        <span class=format!("hs-badge hs-badge--{} hs-badge--soft", tone.class())>
            <Icon name=tone.icon() size=IconSize::Xs />
            {children()}
        </span>
    }
}

/// A labelled figure. `small` is the `sm` size, for values longer than a word or two.
#[component]
pub fn Stat(
    label: &'static str,
    #[prop(into)] value: Signal<String>,
    #[prop(optional)] small: bool,
) -> impl IntoView {
    view! {
        <div class=if small { "hs-stat hs-stat--sm" } else { "hs-stat hs-stat--md" }>
            <p class="hs-stat__label">{label}</p>
            <p class="hs-stat__value">{move || value.get()}</p>
        </div>
    }
}

/// A ledger table; pass `<thead>` and `<tbody>` as children.
#[component]
pub fn Table(#[prop(optional)] caption: Option<&'static str>, children: Children) -> impl IntoView {
    view! {
        <div class="hs-table-wrap">
            <table class="hs-table hs-table--comfortable">
                {caption.map(|c| view! { <caption class="hs-table__caption">{c}</caption> })}
                {children()}
            </table>
        </div>
    }
}

/// A calm, specific empty state in a niche.
#[component]
pub fn Empty(icon: &'static str, title: &'static str, text: &'static str) -> impl IntoView {
    view! {
        <div class="hs-empty hs-empty--sm">
            <div class="hs-empty__niche" aria-hidden="true"><Icon name=icon size=IconSize::Lg /></div>
            <h3 class="hs-empty__title">{title}</h3>
            <p class="hs-empty__text">{text}</p>
        </div>
    }
}

/// Read-only code or data, one span per line.
#[component]
pub fn CodeBlock(
    #[prop(into)] code: String,
    #[prop(optional)] title: Option<&'static str>,
) -> impl IntoView {
    let lines: Vec<String> = code
        .trim_end_matches('\n')
        .lines()
        .map(String::from)
        .collect();
    let numbered = if lines.len() > 1 {
        "hs-code__pre has-lines"
    } else {
        "hs-code__pre"
    };
    view! {
        <figure class="hs-code">
            {title.map(|t| view! {
                <figcaption class="hs-code__bar"><span class="hs-code__title">{t}</span></figcaption>
            })}
            <pre class=numbered tabindex="0">
                <code>{lines.into_iter().map(|l| view! { <span class="hs-code__line">{l}"\n"</span> }).collect_view()}</code>
            </pre>
        </figure>
    }
}

/// The house trio at the top of a page: inscription eyebrow, display title, lead.
#[component]
pub fn PageHead(
    eyebrow: &'static str,
    title: &'static str,
    #[prop(optional)] lead: Option<&'static str>,
) -> impl IntoView {
    view! {
        <header class="console-head">
            <p class="hs-eyebrow">{eyebrow}</p>
            <h1 class="heading-1">{title}</h1>
            {lead.map(|l| view! { <p class="lead">{l}</p> })}
        </header>
    }
}
