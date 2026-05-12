use dioxus::prelude::*;
use dioxus_primitives::dialog::{self, DialogCtx, DialogRootProps, DialogTitleProps};

/// Right-anchored edge panel. We only render the right side today; re-add
/// `Top` / `Bottom` / `Left` variants and their CSS when a caller needs them.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum SheetSide {
    #[default]
    Right,
}

impl SheetSide {
    pub fn as_str(&self) -> &'static str {
        match self {
            SheetSide::Right => "right",
        }
    }
}

#[component]
pub fn Sheet(props: DialogRootProps) -> Element {
    rsx! {
        SheetRoot {
            id: props.id,
            is_modal: props.is_modal,
            open: props.open,
            default_open: props.default_open,
            on_open_change: props.on_open_change,
            attributes: props.attributes,
            {props.children}
        }
    }
}

#[component]
fn SheetRoot(props: DialogRootProps) -> Element {
    rsx! {
        dialog::DialogRoot {
            class: "sheet-root",
            "data-slot": "sheet-root",
            id: props.id,
            is_modal: props.is_modal,
            open: props.open,
            default_open: props.default_open,
            on_open_change: props.on_open_change,
            attributes: props.attributes,
            {props.children}
        }
    }
}

#[component]
pub fn SheetContent(
    #[props(default = ReadSignal::new(Signal::new(None)))] id: ReadSignal<Option<String>>,
    #[props(default)] side: SheetSide,
    #[props(default)] class: Option<String>,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let class = class
        .map(|c| format!("sheet {c}"))
        .unwrap_or("sheet".to_string());

    rsx! {
        dialog::DialogContent {
            class,
            id,
            "data-slot": "sheet-content",
            "data-side": side.as_str(),
            attributes,
            {children}
            SheetClose { class: "sheet-close", "aria-label": "Close",
                "×"
            }
        }
    }
}

#[component]
pub fn SheetHeader(
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    rsx! {
        div { class: "sheet-header", "data-slot": "sheet-header", ..attributes, {children} }
    }
}

#[component]
pub fn SheetFooter(
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    rsx! {
        div { class: "sheet-footer", "data-slot": "sheet-footer", ..attributes, {children} }
    }
}

#[component]
pub fn SheetTitle(props: DialogTitleProps) -> Element {
    rsx! {
        dialog::DialogTitle {
            id: props.id,
            class: "sheet-title",
            "data-slot": "sheet-title",
            attributes: props.attributes,
            {props.children}
        }
    }
}

#[component]
pub fn SheetClose(
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let ctx: DialogCtx = use_context();
    rsx! {
        button {
            onclick: move |_| ctx.set_open(false),
            ..attributes,
            {children}
        }
    }
}
