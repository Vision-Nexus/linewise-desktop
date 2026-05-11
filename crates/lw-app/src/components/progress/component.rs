use dioxus::prelude::*;
use dioxus_primitives::progress::{self, ProgressIndicatorProps, ProgressProps};

/// Thin wrapper around `dioxus_primitives::progress::Progress` that ensures
/// our stylesheet is mounted and that the default class is applied. Any extra
/// class passed via attributes is appended by Dioxus.
#[component]
pub fn Progress(props: ProgressProps) -> Element {
    rsx! {
        document::Link { rel: "stylesheet", href: asset!("./style.css") }
        progress::Progress {
            class: "progress",
            value: props.value,
            max: props.max,
            attributes: props.attributes,
            {props.children}
        }
    }
}

#[component]
pub fn ProgressIndicator(props: ProgressIndicatorProps) -> Element {
    rsx! {
        progress::ProgressIndicator { class: "progress-indicator", attributes: props.attributes, {props.children} }
    }
}
