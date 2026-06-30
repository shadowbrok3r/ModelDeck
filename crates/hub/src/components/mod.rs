//! Shared UI widgets for ModelDeck.

pub mod dialog;

use dioxus::prelude::*;

/// A toggle/filter button; active state uses the nebula accent.
#[component]
pub fn FilterButton(label: String, active: bool, onclick: EventHandler<MouseEvent>) -> Element {
    rsx! {
        button {
            class: if active { "btn-nebula" } else { "btn-cosmic" },
            onclick: move |e| onclick.call(e),
            "{label}"
        }
    }
}

/// A small labelled stat chip used in the nav and system view.
#[component]
pub fn StatPill(label: String, value: String) -> Element {
    rsx! {
        div { class: "flex flex-col",
            span { class: "text-star-white font-semibold", "{value}" }
            span { class: "text-stardust text-xs", "{label}" }
        }
    }
}

/// A coloured status badge.
#[component]
pub fn Badge(text: String, kind: String) -> Element {
    let cls = match kind.as_str() {
        "good" => "badge badge-nebula",
        "warn" => "badge badge-method",
        "bad" => "badge",
        _ => "badge badge-method",
    };
    rsx! { span { class: "{cls}", "{text}" } }
}
