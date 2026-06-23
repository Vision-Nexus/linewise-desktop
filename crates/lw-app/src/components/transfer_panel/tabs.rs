//! Tab model + bucketing for the transfer panel.
//!
//! The panel has a primary three-way split — In Progress / Completed /
//! Failed — and a secondary Quality / Network split under Failed. The
//! per-tab counts ARE the summary (rendered in the tab labels), so the
//! bucketing here is the single source of truth for both "which list does
//! this row belong to" and "what number shows in the tab".
//!
//! Bucketing is derived from [`UploadState`] only; it never reads the
//! progress maps. Keeping it pure means the tab strip can recompute counts
//! from a borrow of the task list without cloning.

use dioxus::prelude::*;
use lw_core::models::UploadState;

/// Primary tab of the transfer panel.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PrimaryTab {
    InProgress,
    Completed,
    Failed,
}

/// Secondary tab under Failed: quality rejections vs. transport failures.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FailedTab {
    Quality,
    Network,
}

impl PrimaryTab {
    /// True when a row in `state` belongs under this primary tab.
    ///
    /// * **In Progress** — everything the engine is still working on, plus
    ///   the pre-upload staging states (Checking, Hashing, Staged) and
    ///   Paused, which the user expects to see where they left it.
    /// * **Completed** — terminal success. Reconciled duplicates also land
    ///   here (see the `DuplicateDetected` arm in `upload_runtime.rs`),
    ///   because a duplicate means the content is already stored.
    /// * **Failed** — `Rejected` (quality) and `Failed` (network).
    pub fn contains(self, state: &UploadState) -> bool {
        match self {
            PrimaryTab::InProgress => matches!(
                state,
                UploadState::QualityChecking
                    | UploadState::Hashing
                    | UploadState::Staged
                    | UploadState::Pending
                    | UploadState::Validating
                    | UploadState::Transcoding
                    | UploadState::Creating
                    | UploadState::Uploading
                    | UploadState::Verifying
                    | UploadState::Paused
            ),
            PrimaryTab::Completed => matches!(state, UploadState::Completed),
            PrimaryTab::Failed => {
                matches!(state, UploadState::Rejected | UploadState::Failed)
            }
        }
    }
}

/// CSS for the horizontal tab strip. Class-toggled rather than inline so
/// the active-state background/border don't get stuck across re-renders —
/// the same lesson `settings_modal.rs` learned for its vertical rail.
pub const TRANSFER_TAB_CSS: &str = r#"
.lw-transfer-tab {
    background: transparent;
    border: none;
    border-bottom: 2px solid transparent;
    cursor: pointer;
    padding: 8px 14px;
    font-size: 13px;
    font-weight: 500;
    color: var(--text-secondary);
    transition: color 0.15s, border-color 0.15s;
}
.lw-transfer-tab:hover { color: var(--text); }
.lw-transfer-tab.is-active {
    color: var(--text);
    font-weight: 600;
    border-bottom-color: var(--btn-primary);
}
.lw-subtab {
    background: transparent;
    border: 1px solid var(--border);
    cursor: pointer;
    padding: 4px 10px;
    font-size: 12px;
    font-weight: 500;
    color: var(--text-secondary);
    border-radius: 14px;
    transition: color 0.15s, background 0.15s, border-color 0.15s;
}
.lw-subtab.is-active {
    color: white;
    background: var(--btn-primary);
    border-color: var(--btn-primary);
}
.lw-upload-menu-item {
    display: block;
    width: 100%;
    text-align: left;
    padding: 8px 10px;
    font-size: 13px;
    border-radius: 4px;
    background: transparent;
    color: var(--text);
    border: none;
    cursor: pointer;
    transition: background 0.15s;
}
.lw-upload-menu-item:hover { background: var(--bg-tertiary); }
"#;

/// One button in the primary tab strip. Label carries its own count, e.g.
/// "In Progress 3".
#[component]
pub fn PrimaryTabButton(
    label: String,
    count: usize,
    active: bool,
    onclick: EventHandler<MouseEvent>,
) -> Element {
    let class = if active {
        "lw-transfer-tab is-active"
    } else {
        "lw-transfer-tab"
    };
    rsx! {
        button {
            class: "{class}",
            onclick: move |e| onclick.call(e),
            "{label} "
            span {
                style: "font-size: 11px; color: var(--text-secondary); background: var(--bg-tertiary); padding: 1px 7px; border-radius: 10px; margin-left: 2px;",
                "{count}"
            }
        }
    }
}

/// One pill in the secondary (Quality / Network) sub-tab strip.
#[component]
pub fn SubTabButton(
    label: String,
    count: usize,
    active: bool,
    onclick: EventHandler<MouseEvent>,
) -> Element {
    let class = if active {
        "lw-subtab is-active"
    } else {
        "lw-subtab"
    };
    rsx! {
        button {
            class: "{class}",
            onclick: move |e| onclick.call(e),
            "{label} ({count})"
        }
    }
}
