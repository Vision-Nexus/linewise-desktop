//! Local-data wipe primitives for the title-bar Repair affordance.
//!
//! Three independent slices the user can opt into:
//!   * **logs** — every file under `AppConfig::data_dir()/logs/`. The
//!     tracing-appender holds today's file open; on macOS/Linux the unlink
//!     succeeds while it stays open, but the appender keeps writing to the
//!     deleted inode until the next rotation or restart. The Repair flow
//!     always asks for an app restart afterwards, so the inode is released
//!     promptly.
//!   * **config** — `AppConfig::config_path()`. The next `AppConfig::load`
//!     re-creates a default `config.toml`, so removing the file is enough.
//!   * **db** — the SQLite file plus its WAL/SHM sidecars. Reuses
//!     [`crate::db::Database::reset_local_files`] verbatim.
//!
//! Each slice runs independently and reports its own outcome — a missing
//! logs directory, for instance, must not block a config wipe. The caller
//! collects the per-slice results and decides whether to surface a partial
//! failure.

use crate::config::AppConfig;
use crate::db::Database;
use crate::error::RepairError;
use crate::logging;

/// Which on-disk slices to wipe. Independently set, all three optional.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RepairSelection {
    pub logs: bool,
    pub config: bool,
    pub db: bool,
}

impl RepairSelection {
    pub fn any(&self) -> bool {
        self.logs || self.config || self.db
    }
}

/// Per-slice outcome. The caller can render one row per requested slice.
#[derive(Debug)]
pub struct RepairOutcome {
    pub logs: Option<Result<(), RepairError>>,
    pub config: Option<Result<(), RepairError>>,
    pub db: Option<Result<(), RepairError>>,
}

impl RepairOutcome {
    pub fn all_ok(&self) -> bool {
        let ok = |r: &Option<Result<(), RepairError>>| r.as_ref().is_none_or(|r| r.is_ok());
        ok(&self.logs) && ok(&self.config) && ok(&self.db)
    }
}

/// Run the requested slices in order: logs → config → db. Order matters
/// only insofar as the db wipe is the most disruptive (closes the open
/// connection's files); putting it last keeps the earlier wipes from
/// racing against a still-running query.
#[tracing::instrument(skip_all, fields(
    logs = selection.logs,
    config = selection.config,
    db = selection.db,
))]
pub fn run(selection: RepairSelection) -> RepairOutcome {
    tracing::warn!("repair: starting destructive wipe");
    RepairOutcome {
        logs: selection.logs.then(wipe_logs),
        config: selection.config.then(wipe_config),
        db: selection.db.then(wipe_db),
    }
}

/// Remove every file under `log_dir()`. Returns `Ok(())` when the
/// directory does not exist — there is nothing to wipe and that is the
/// successful end state, not an error.
fn wipe_logs() -> Result<(), RepairError> {
    let dir = logging::log_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(path = %dir.display(), "repair: logs directory missing, nothing to wipe");
            return Ok(());
        }
        Err(e) => {
            return Err(RepairError::Io {
                path: dir,
                source: e,
            });
        }
    };
    for entry in entries {
        let entry = entry.map_err(|e| RepairError::Io {
            path: dir.clone(),
            source: e,
        })?;
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|e| RepairError::Io {
                path: path.clone(),
                source: e,
            })?
            .is_file()
        {
            // Skip subdirectories — the appender only writes flat files.
            // If the user dropped something else here, leave it alone.
            continue;
        }
        if let Err(e) = std::fs::remove_file(&path) {
            return Err(RepairError::Io { path, source: e });
        }
    }
    tracing::warn!("repair: logs wiped");
    Ok(())
}

fn wipe_config() -> Result<(), RepairError> {
    let path = AppConfig::config_path();
    match std::fs::remove_file(&path) {
        Ok(()) => {
            tracing::warn!(path = %path.display(), "repair: config wiped");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(path = %path.display(), "repair: config file already absent");
            Ok(())
        }
        Err(e) => Err(RepairError::Io { path, source: e }),
    }
}

fn wipe_db() -> Result<(), RepairError> {
    Database::reset_local_files().map_err(RepairError::Db)?;
    tracing::warn!("repair: db files wiped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_selection_runs_no_branches() {
        let outcome = run(RepairSelection::default());
        assert!(outcome.logs.is_none());
        assert!(outcome.config.is_none());
        assert!(outcome.db.is_none());
        assert!(outcome.all_ok());
    }

    #[test]
    fn any_returns_true_only_when_a_slice_is_set() {
        assert!(!RepairSelection::default().any());
        assert!(
            RepairSelection {
                logs: true,
                config: false,
                db: false
            }
            .any()
        );
        assert!(
            RepairSelection {
                logs: false,
                config: true,
                db: false
            }
            .any()
        );
        assert!(
            RepairSelection {
                logs: false,
                config: false,
                db: true
            }
            .any()
        );
    }
}
