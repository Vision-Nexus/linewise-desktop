//! Repair affordance — wipes selected on-disk slices (logs, config,
//! sqlite db) after the user types a confirmation phrase. Triggered
//! from the wrench button at the top-left of the title bar (next to
//! the logo, deliberately outside the auth gate) so a wedged app stays
//! reachable even before sign-in completes. The modal itself is mounted
//! at the app shell — above the AuthedShell/MainView split — so the
//! same button works during boot, on the login page, and post-auth.
//!
//! Why a modal and not a settings pane: settings reads from config, and
//! we want this reachable even when config is the thing that's broken.
//!
//! Flow: user picks one or more slices, types `RESET`, clicks Run. The
//! handler signs out (clearing the keyring), takes ownership of the
//! `CoreServices` bundle out of `AppState` and drops it explicitly so
//! the SQLite pool releases its WAL/SHM file locks, runs the wipe via
//! `lw_core::repair::run` on a blocking task, then bumps `restart_token`
//! so the boot effect rebuilds `CoreServices` against whatever survived
//! (or was just reset to defaults). Restart fires unconditionally — even
//! a partial failure leaves the app with `services: None` and an empty
//! auth state, so the boot effect must re-run to restore coherence.

use crate::state::CoreServices;
use crate::state::{AppState, ToastKind};
use dioxus::prelude::*;
use lw_core::config::AppConfig;
use lw_core::error::RepairError;
use lw_core::logging::log_dir;
use lw_core::repair::{RepairOutcome, RepairSelection, run as run_repair};
use std::sync::Arc;

/// The literal string the user must type to enable the Run button. Kept
/// English so the modal doesn't have to localize the input itself; the
/// label and helper text around it are localizable freely.
const CONFIRMATION_PHRASE: &str = "RESET";

#[component]
pub fn RepairModal(on_close: EventHandler<()>) -> Element {
    let close = on_close;
    let mut selection = use_signal(RepairSelection::default);
    let mut phrase = use_signal(String::new);
    let mut running = use_signal(|| false);
    let mut last_outcome: Signal<Option<RepairOutcome>> = use_signal(|| None);

    let phrase_matches = *phrase.read() == CONFIRMATION_PHRASE;
    let any_selected = selection.read().any();
    let is_running = *running.read();
    let can_run = phrase_matches && any_selected && !is_running;

    let app_state_run = use_context::<AppState>();
    let on_run = move |_| {
        if !phrase_matches || !any_selected || is_running {
            return;
        }
        running.set(true);
        let chosen = *selection.read();
        let mut app_state = app_state_run.clone();
        spawn(async move {
            // Sign out first so the keyring entry doesn't outlive the
            // wiped DB / config. We `take()` the services slot to
            // transfer ownership of the Arc bundle out of `AppState`
            // before signing out, then drop it explicitly *before* the
            // blocking wipe. If we left services in `AppState`, the
            // SqlitePool inside `Arc<Database>` would keep WAL/SHM file
            // handles open while `wipe_db` runs and SQLite can recreate
            // those sidecars mid-wipe — re-corrupting the very files we
            // just removed.
            let services = app_state.services.write().take();
            if let Some(svcs) = &services {
                svcs.auth.sign_out().await;
            }
            // Reset auth-derived signals next to the sign-out, mirroring
            // SignOutButton.
            app_state.is_authenticated.set(false);
            app_state.user_info.set(None);
            app_state.selected_tenant.set(None);
            app_state.selected_project.set(None);
            app_state.projects.set(Vec::new());
            app_state.tenant_projects.write().clear();
            app_state.upload_tasks.set(Vec::new());

            // Tear down the long-running auto-retry worker, then drop
            // the local Arc bundle. The auto-retry task holds
            // `Arc<UploadEngine>` (and therefore `Arc<Database>`); if
            // we drop `services` without aborting it first, the
            // SqlitePool stays alive in the runtime and SQLite can
            // recreate WAL/SHM sidecars mid-wipe — exactly the bug
            // `Database::reset_local_files` warned about.
            shutdown_services(services).await;

            // Run the file-system wipes on a blocking task — they're
            // synchronous I/O and we don't want to stall the runtime.
            let outcome = match tokio::task::spawn_blocking(move || run_repair(chosen)).await {
                Ok(o) => o,
                Err(join_err) => {
                    // Honest reporting: we can't tell which slices ran.
                    // Mark every selected slice as "unknown state" via
                    // RepairError::TaskPanicked rather than fabricating
                    // a per-path I/O failure that didn't actually happen.
                    tracing::error!("repair task did not complete: {join_err}");
                    let reason = join_err.to_string();
                    let mark = |selected: bool| -> Option<Result<(), RepairError>> {
                        selected.then(|| {
                            Err(RepairError::TaskPanicked {
                                reason: reason.clone(),
                            })
                        })
                    };
                    RepairOutcome {
                        logs: mark(chosen.logs),
                        config: mark(chosen.config),
                        db: mark(chosen.db),
                    }
                }
            };

            let all_ok = outcome.all_ok();
            last_outcome.set(Some(outcome));
            running.set(false);

            // Always bump restart_token: services are already None and
            // signals are reset, so the boot effect must re-run to
            // restore a coherent app state — even on partial failure,
            // where we'd otherwise leave the app wedged with no DB and
            // no services.
            app_state.request_restart();

            if all_ok {
                app_state.show_toast("Repair complete — restarting", ToastKind::Success);
                close.call(());
            } else {
                app_state.show_toast(
                    "Repair finished with errors — restarting; see modal for details",
                    ToastKind::Error,
                );
            }
        });
    };

    rsx! {
        div {
            style: "position: fixed; inset: 0; background: rgba(0,0,0,0.4); z-index: 100; \
                    display: flex; align-items: center; justify-content: center;",
            onclick: move |_| if !is_running { close.call(()) },

            div {
                style: "background: var(--bg); border: 1px solid var(--border); border-radius: 8px; \
                        width: 520px; max-width: 92vw; max-height: 85vh; \
                        display: flex; flex-direction: column; \
                        color: var(--text); box-shadow: var(--shadow-md); overflow: hidden;",
                onclick: move |e| e.stop_propagation(),
                onmousedown: move |e| e.stop_propagation(),

                // Header — destructive accent on the title makes the intent
                // unmistakable from the moment the modal opens.
                div {
                    style: "display: flex; justify-content: space-between; align-items: center; \
                            padding: 16px 20px; border-bottom: 1px solid var(--border); flex-shrink: 0;",
                    h2 {
                        class: "text-destructive",
                        style: "margin: 0; font-size: 18px; font-weight: 600;",
                        "Repair: reset local app data"
                    }
                    button {
                        style: "background: none; border: none; color: var(--text-muted); \
                                cursor: pointer; font-size: 20px; padding: 4px; line-height: 1;",
                        disabled: is_running,
                        onclick: move |_| close.call(()),
                        "×"
                    }
                }

                // Body
                div {
                    style: "flex: 1; min-height: 0; overflow-y: auto; padding: 20px; \
                            display: flex; flex-direction: column; gap: 16px;",

                    p {
                        style: "margin: 0; font-size: 13px; color: var(--text-secondary); line-height: 1.55;",
                        "This wipes the selected on-disk data. The app signs you out and \
                         restarts to rebuild against whatever remains. Uploaded data on the \
                         server is not affected."
                    }

                    SelectionRow {
                        label: "Logs",
                        description: "All log files under the data directory.",
                        path_hint: format!("{}", log_dir().display()),
                        checked: selection.read().logs,
                        disabled: is_running,
                        on_toggle: move |v| selection.write().logs = v,
                    }
                    SelectionRow {
                        label: "Config file",
                        description: "Resets settings to defaults on next launch.",
                        path_hint: format!("{}", AppConfig::config_path().display()),
                        checked: selection.read().config,
                        disabled: is_running,
                        on_toggle: move |v| selection.write().config = v,
                    }
                    SelectionRow {
                        label: "SQLite database",
                        description: "Clears the upload queue and dedup cache.",
                        path_hint: format!("{}", AppConfig::db_path().display()),
                        checked: selection.read().db,
                        disabled: is_running,
                        on_toggle: move |v| selection.write().db = v,
                    }

                    // Confirmation phrase
                    div {
                        style: "display: flex; flex-direction: column; gap: 6px;",
                        label {
                            style: "font-size: 12px; color: var(--text-secondary);",
                            "Type "
                            code {
                                style: "background: var(--bg-secondary); padding: 1px 6px; \
                                        border-radius: 4px; font-family: var(--font-mono); \
                                        color: var(--text);",
                                "{CONFIRMATION_PHRASE}"
                            }
                            " to confirm."
                        }
                        input {
                            r#type: "text",
                            value: "{phrase}",
                            placeholder: "{CONFIRMATION_PHRASE}",
                            disabled: is_running,
                            autocomplete: "off",
                            spellcheck: "false",
                            style: "width: 100%; padding: 8px 10px; border-radius: 6px; \
                                    border: 1px solid var(--border); background: var(--bg); \
                                    color: var(--text); font-family: var(--font-mono); \
                                    font-size: 13px;",
                            oninput: move |e| phrase.set(e.value()),
                        }
                    }

                    // Per-slice outcome rows after a run
                    if let Some(outcome) = last_outcome.read().as_ref() {
                        OutcomeList { outcome: format_outcome(outcome) }
                    }
                }

                // Footer
                div {
                    style: "display: flex; justify-content: flex-end; gap: 8px; \
                            padding: 12px 20px; border-top: 1px solid var(--border); \
                            background: var(--bg-secondary); flex-shrink: 0;",
                    button {
                        class: "px-4 py-2 text-sm rounded border border-border bg-background hover:bg-accent disabled:opacity-50 disabled:cursor-not-allowed",
                        disabled: is_running,
                        onclick: move |_| close.call(()),
                        "Cancel"
                    }
                    button {
                        class: "px-4 py-2 text-sm rounded bg-destructive text-destructive-foreground hover:opacity-90 disabled:opacity-50 disabled:cursor-not-allowed",
                        disabled: !can_run,
                        onclick: on_run,
                        if is_running {
                            span { class: "spinner spinner-sm mr-1" }
                            "Running…"
                        } else {
                            "Run repair"
                        }
                    }
                }
            }
        }
    }
}

/// Quiesce every long-running task that might hold a clone of the
/// service Arcs, then drop the bundle. Two-phase:
///
///   1. Abort the auto-retry worker. It is the only task spawned with
///      the engine handle that lives forever; aborting it releases its
///      `Arc<UploadEngine>` (and transitively `Arc<Database>`).
///   2. Log the strong counts of the load-bearing Arcs *before* the
///      drop. `db` is the primary signal — it gates the WAL/SHM file
///      lock — but we also log `upload_engine` and `api` so a future
///      leak in either path shows up in traces. A count > 1 here means
///      a task we don't know about is still holding a clone, and the
///      blocking wipe that follows will race against an open pool.
async fn shutdown_services(services: Option<CoreServices>) {
    let Some(svcs) = services else {
        return;
    };

    // Phase 1: cancel the auto-retry worker.
    let handle = svcs.auto_retry_handle.lock().await.take();
    if let Some(h) = handle {
        h.abort();
        // `abort` is best-effort; awaiting the handle ensures the task
        // has actually unwound and dropped its `Arc<UploadEngine>`
        // before we move on. A `JoinError::cancelled` is the expected
        // outcome and isn't a failure.
        match h.await {
            Ok(()) => tracing::warn!("repair: auto-retry task ended cleanly"),
            Err(e) if e.is_cancelled() => {
                tracing::info!("repair: auto-retry task cancelled")
            }
            Err(e) => tracing::warn!("repair: auto-retry task ended with error: {e}"),
        }
    }

    // Phase 2: log strong counts before drop. We expect 1 across the
    // board after the abort settles — anything higher is a real leak
    // worth investigating, not a benign reference.
    let db_strong = Arc::strong_count(&svcs.db);
    let engine_strong = Arc::strong_count(&svcs.upload_engine);
    let api_strong = Arc::strong_count(&svcs.api);
    if db_strong > 1 || engine_strong > 1 || api_strong > 1 {
        tracing::warn!(
            db_strong,
            engine_strong,
            api_strong,
            "repair: extra Arc references survive shutdown — a background task \
             still holds a service handle and the SQLite pool may stay open \
             across the wipe"
        );
    } else {
        tracing::info!(
            db_strong,
            engine_strong,
            api_strong,
            "repair: service Arcs released cleanly"
        );
    }
    drop(svcs);
}

#[component]
fn SelectionRow(
    label: &'static str,
    description: &'static str,
    path_hint: String,
    checked: bool,
    disabled: bool,
    on_toggle: EventHandler<bool>,
) -> Element {
    rsx! {
        label {
            style: "display: flex; gap: 10px; padding: 10px 12px; border-radius: 6px; \
                    border: 1px solid var(--border); cursor: pointer; align-items: flex-start;",
            input {
                r#type: "checkbox",
                checked,
                disabled,
                style: "margin-top: 3px;",
                onchange: move |e| on_toggle.call(e.value() == "true"),
            }
            div {
                style: "display: flex; flex-direction: column; gap: 2px; min-width: 0;",
                span {
                    style: "font-size: 13px; font-weight: 600; color: var(--text);",
                    "{label}"
                }
                span {
                    style: "font-size: 12px; color: var(--text-secondary);",
                    "{description}"
                }
                span {
                    style: "font-size: 11px; color: var(--text-muted); \
                            font-family: var(--font-mono); word-break: break-all;",
                    "{path_hint}"
                }
            }
        }
    }
}

/// One row per requested slice: ✓ on success, ✗ + reason on failure.
/// Skipped slices (not selected) don't appear here.
#[derive(Clone, PartialEq)]
struct OutcomeRow {
    label: &'static str,
    ok: bool,
    detail: String,
}

fn format_outcome(outcome: &RepairOutcome) -> Vec<OutcomeRow> {
    let mut rows = Vec::new();
    let mut push = |label: &'static str, slot: &Option<Result<(), RepairError>>| {
        if let Some(result) = slot {
            rows.push(match result {
                Ok(()) => OutcomeRow {
                    label,
                    ok: true,
                    detail: "wiped".to_string(),
                },
                Err(e) => OutcomeRow {
                    label,
                    ok: false,
                    detail: e.to_string(),
                },
            });
        }
    };
    push("Logs", &outcome.logs);
    push("Config", &outcome.config);
    push("Database", &outcome.db);
    rows
}

#[component]
fn OutcomeList(outcome: Vec<OutcomeRow>) -> Element {
    rsx! {
        div {
            style: "display: flex; flex-direction: column; gap: 6px; \
                    border-top: 1px solid var(--border); padding-top: 12px;",
            span {
                style: "font-size: 12px; font-weight: 600; color: var(--text-secondary);",
                "Last run"
            }
            for row in outcome.iter() {
                div {
                    key: "{row.label}",
                    style: "display: flex; gap: 8px; align-items: flex-start; font-size: 12px;",
                    span {
                        style: if row.ok {
                            "color: var(--success); font-weight: 700; min-width: 14px;"
                        } else {
                            "color: var(--error); font-weight: 700; min-width: 14px;"
                        },
                        if row.ok { "✓" } else { "✗" }
                    }
                    div {
                        style: "display: flex; flex-direction: column; min-width: 0;",
                        span {
                            style: "color: var(--text); font-weight: 500;",
                            "{row.label}"
                        }
                        span {
                            style: "color: var(--text-secondary); word-break: break-word;",
                            "{row.detail}"
                        }
                    }
                }
            }
        }
    }
}
