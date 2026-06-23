//! Right-side sheet for io.visionlab capture metadata, in two modes:
//!
//! - **Batch** (`task_id == None`): applies to every clip already in the queue
//!   (`apply_capture_to_staged`) and is recorded as the default for files added
//!   later (`set_batch_capture_metadata`).
//! - **Per-file** (`task_id == Some(id)`): records that one clip's metadata
//!   (`set_capture_metadata`). This is the fill the per-row "Add metadata" opens.
//!
//! Both modes only **record** the values; the upload step is manual. A clip with
//! no metadata holds `Staged` (showing "Needs metadata") and the manual "Upload"
//! (`confirm_staged`) skips it; a filled clip shows "✓ filled" and uploads on that
//! click. `process_task` embeds the values into the MP4 before upload.
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
pub fn CaptureMetadataDialog(
    open: bool,
    /// `Some(task_id)` = per-file fill (records that clip's metadata);
    /// `None` = batch (applies to the whole queue + default for later files).
    task_id: Option<String>,
    on_close: EventHandler<bool>,
) -> Element {
    let services = use_context::<CoreServices>();
    let engine = services.upload_engine.clone();
    // Bumped on save so staged rows re-render their "✓ filled" / "Needs metadata"
    // state immediately (the engine's capture map is not itself reactive).
    let mut capture_rev = use_context::<crate::state::AppState>().capture_rev;

    let mut form = use_signal(CaptureForm::default);
    let mut error = use_signal(|| Option::<String>::None);

    // Reset the buffer each time the sheet opens so a reused dialog never shows a
    // previous clip's values. Per-file prefers the clip's own entry, then the
    // batch default; batch mode shows the current default.
    {
        let engine_eff = engine.clone();
        let task_eff = task_id.clone();
        use_effect(move || {
            if open {
                let initial = match &task_eff {
                    Some(id) => engine_eff
                        .capture_metadata_for(id)
                        .or_else(|| engine_eff.batch_capture_metadata())
                        .unwrap_or_default(),
                    None => engine_eff.batch_capture_metadata().unwrap_or_default(),
                };
                form.set(CaptureForm::from(initial));
                error.set(None);
            }
        });
    }

    let engine_save = engine.clone();
    let task_save = task_id.clone();
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
        match &task_save {
            // Per-file: record this clip's metadata. It stays `Staged` showing
            // "✓ filled"; the manual "Upload" step dispatches it.
            Some(id) => {
                if meta.is_empty() {
                    error.set(Some("Fill at least one field before saving.".to_string()));
                    return;
                }
                engine_save.set_capture_metadata(id, Some(meta));
                capture_rev += 1;
            }
            // Batch: apply to every clip already in the queue AND keep as the
            // default for files added later.
            None => {
                engine_save.set_batch_capture_metadata((!meta.is_empty()).then_some(meta.clone()));
                if !meta.is_empty() {
                    let engine = engine_save.clone();
                    spawn(async move {
                        engine.apply_capture_to_staged(meta).await;
                        capture_rev += 1;
                    });
                }
            }
        }
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
                    SheetTitle {
                        if task_id.is_some() { "Fill Capture Metadata" } else { "Capture Metadata Defaults" }
                    }
                    div {
                        style: "font-size: 12px; color: var(--text-secondary); margin-top: 4px;",
                        if task_id.is_some() {
                            "Required before this clip uploads. Embedded into the file, then the clip is queued."
                        } else {
                            "Applied to all files in the queue now and any you add next. Embedded into each file on upload."
                        }
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
                        if task_id.is_some() { "Save & Upload" } else { "Save" }
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
