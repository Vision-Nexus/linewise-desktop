//! Right-side sheet for the **batch** io.visionlab capture metadata applied to
//! every file staged afterwards ("set defaults → add files"). Saving stores the
//! values on the upload engine (`set_batch_capture_metadata`, in-memory); each
//! file staged while defaults are set carries them, and `process_task` embeds
//! them into the MP4 before upload. Capture is optional (soft) — leaving fields
//! blank just uploads untagged.
//!
//! Mounted unconditionally by `TransferPanel` so the slide animation plays on
//! close; visibility is driven by the `open` prop.

use crate::components::sheet::{
    Sheet, SheetContent, SheetFooter, SheetHeader, SheetSide, SheetTitle,
};
use crate::state::CoreServices;
use dioxus::prelude::*;
use lw_core::capture::{CaptureMetadata, canonicalize_operator, validate_fov};

/// All-string edit buffer (fov kept as text while typing). Converted to a typed
/// [`CaptureMetadata`] on save, where operator/fov are validated.
#[derive(Clone, Default)]
struct CaptureForm {
    country: String,
    city: String,
    site: String,
    station: String,
    operator: String,
    make: String,
    model: String,
    fov: String,
    action: String,
}

impl From<CaptureMetadata> for CaptureForm {
    fn from(m: CaptureMetadata) -> Self {
        Self {
            country: m.country.unwrap_or_default(),
            city: m.city.unwrap_or_default(),
            site: m.site.unwrap_or_default(),
            station: m.station.unwrap_or_default(),
            operator: m.operator.unwrap_or_default(),
            make: m.make.unwrap_or_default(),
            model: m.model.unwrap_or_default(),
            fov: m.fov.map(|n| n.to_string()).unwrap_or_default(),
            action: m.action.unwrap_or_default(),
        }
    }
}

fn opt(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

#[component]
pub fn CaptureMetadataDialog(open: bool, on_close: EventHandler<bool>) -> Element {
    let services = use_context::<CoreServices>();
    let engine = services.upload_engine.clone();

    let initial = engine.batch_capture_metadata().unwrap_or_default();
    let mut form = use_signal(|| CaptureForm::from(initial));
    let mut error = use_signal(|| Option::<String>::None);

    let engine_save = engine.clone();
    let on_save = move |_| {
        let f = form.read().clone();

        // Validate operator + fov (mirror backend); blank = omitted.
        let operator = match opt(&f.operator) {
            None => None,
            Some(v) => match canonicalize_operator(&v) {
                Ok(c) => Some(c),
                Err(e) => {
                    error.set(Some(e));
                    return;
                }
            },
        };
        let fov = match opt(&f.fov) {
            None => None,
            Some(v) => match v
                .parse::<i32>()
                .map_err(|_| format!("FOV must be a number: '{v}'"))
                .and_then(validate_fov)
            {
                Ok(n) => Some(n),
                Err(e) => {
                    error.set(Some(e));
                    return;
                }
            },
        };

        let meta = CaptureMetadata {
            country: opt(&f.country),
            city: opt(&f.city),
            site: opt(&f.site),
            station: opt(&f.station),
            operator,
            make: opt(&f.make),
            model: opt(&f.model),
            fov,
            action: opt(&f.action),
        };
        error.set(None);
        engine_save.set_batch_capture_metadata((!meta.is_empty()).then_some(meta));
        on_close.call(true);
    };

    let label_style = "font-size: 12px; font-weight: 500; color: var(--text); margin-bottom: 4px; display: block;";
    let input_style = "width: 100%; padding: 6px 8px; border-radius: 6px; border: 1px solid var(--border); background: var(--bg); color: var(--text); font-size: 13px; box-sizing: border-box;";

    rsx! {
        Sheet {
            open,
            on_open_change: move |is_open: bool| {
                if !is_open {
                    on_close.call(false);
                }
            },
            SheetContent {
                side: SheetSide::Right,
                SheetHeader {
                    SheetTitle { "Capture Metadata" }
                    div {
                        style: "font-size: 12px; color: var(--text-secondary); margin-top: 4px;",
                        "Applied to every file added after saving. Embedded into the file on upload."
                    }
                }

                div {
                    class: "sheet-body",

                    div { style: "margin-bottom: 10px;",
                        label { style: label_style, "Country" }
                        input { style: input_style, placeholder: "Thailand",
                            value: "{form.read().country}",
                            oninput: move |e| form.write().country = e.value() }
                    }
                    div { style: "margin-bottom: 10px;",
                        label { style: label_style, "City" }
                        input { style: input_style, placeholder: "Bangkok",
                            value: "{form.read().city}",
                            oninput: move |e| form.write().city = e.value() }
                    }
                    div { style: "margin-bottom: 10px;",
                        label { style: label_style, "Site" }
                        input { style: input_style, placeholder: "AutomotiveSiliconeParts01",
                            value: "{form.read().site}",
                            oninput: move |e| form.write().site = e.value() }
                    }
                    div { style: "margin-bottom: 10px;",
                        label { style: label_style, "Station" }
                        input { style: input_style, placeholder: "PowderCoatingBooth",
                            value: "{form.read().station}",
                            oninput: move |e| form.write().station = e.value() }
                    }
                    div { style: "margin-bottom: 10px;",
                        label { style: label_style, "Operator (3 digits)" }
                        input { style: input_style, placeholder: "001",
                            value: "{form.read().operator}",
                            oninput: move |e| form.write().operator = e.value() }
                    }
                    div { style: "margin-bottom: 10px;",
                        label { style: label_style, "Device make" }
                        input { style: input_style, placeholder: "DJI",
                            value: "{form.read().make}",
                            oninput: move |e| form.write().make = e.value() }
                    }
                    div { style: "margin-bottom: 10px;",
                        label { style: label_style, "Device model" }
                        input { style: input_style, placeholder: "Osmo Nano",
                            value: "{form.read().model}",
                            oninput: move |e| form.write().model = e.value() }
                    }
                    div { style: "margin-bottom: 10px;",
                        label { style: label_style, "FOV (degrees)" }
                        input { style: input_style, placeholder: "143", r#type: "number",
                            value: "{form.read().fov}",
                            oninput: move |e| form.write().fov = e.value() }
                    }
                    div { style: "margin-bottom: 10px;",
                        label { style: label_style, "Action" }
                        input { style: input_style, placeholder: "Pressing piston rings into cylinder bore",
                            value: "{form.read().action}",
                            oninput: move |e| form.write().action = e.value() }
                    }

                    if let Some(err) = error() {
                        div {
                            style: "margin-top: 4px; font-size: 12px; color: var(--error);",
                            "{err}"
                        }
                    }
                }

                SheetFooter {
                    button {
                        style: "flex: 1; padding: 7px 14px; border-radius: 6px; border: none; background: var(--btn-primary); color: white; cursor: pointer; font-weight: 500; font-size: 13px;",
                        onclick: on_save,
                        "Save"
                    }
                    button {
                        style: "padding: 7px 14px; border-radius: 6px; border: 1px solid var(--border); background: transparent; color: var(--text-secondary); cursor: pointer; font-size: 13px;",
                        onclick: move |_| on_close.call(false),
                        "Cancel"
                    }
                }
            }
        }
    }
}
