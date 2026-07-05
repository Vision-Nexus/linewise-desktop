//! Pure, I/O-free upload-state transition rules — the single source of truth for
//! which [`UploadState`] moves are legal.
//!
//! Every guarded DB write derives its `WHERE state IN (...)` predecessor set from
//! here (see [`predecessors`]) so the SQL guard and this table can never drift.
//! Because the whole thing is pure, the state machine is exhaustively
//! unit-testable with no database — which is the point: the concurrency
//! correctness of the upload engine rests on these transitions being the only
//! legal ones, and that claim is checked by [`tests::allowed_is_total`] over the
//! full `UploadState` cartesian product.
//!
//! Concurrency model (see also `db::Database::settle_completed` / `settle_failure`
//! and `upload::UploadEngine`'s in-process single-flight guard): task ownership
//! within a process is enforced by the single-flight guard; the single-instance
//! guard (`lw-app/src/single_instance.rs`) prevents a second process from sharing
//! the SQLite DB. The transition rules here back the *durable* half — every state
//! write is guarded so a stale or racing writer cannot revert a terminal row or
//! drive an illegal move even if the two guards above ever degrade.

use crate::models::UploadState;

impl UploadState {
    /// Terminal states: no worker may transition OUT of them. Only a user action
    /// (re-adding a file mints a brand-new row; a manual retry is modeled as the
    /// `Failed -> Pending` edge) puts a finished row back into the pipeline.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Rejected | Self::GaveUp)
    }

    /// A row a worker may CLAIM to (re)start processing: idle (`Pending`) or a
    /// previously-failed row awaiting auto/manual retry (`Failed`).
    pub fn is_claimable(&self) -> bool {
        matches!(self, Self::Pending | Self::Failed)
    }

    /// Every variant, for exhaustive iteration in tests and in [`predecessors`].
    /// Kept honest by [`tests::all_covers_every_variant`] (as_str/parse round-trip).
    pub const ALL: [UploadState; 14] = [
        Self::QualityChecking,
        Self::Hashing,
        Self::Staged,
        Self::Rejected,
        Self::Pending,
        Self::Validating,
        Self::Transcoding,
        Self::Creating,
        Self::Uploading,
        Self::Verifying,
        Self::Completed,
        Self::Failed,
        Self::GaveUp,
        Self::Paused,
    ];
}

/// Whether the engine may move a row from `from` to `to`. Pure and total.
///
/// Matched exhaustively over `from` (no `_` catch-all) so adding a new
/// `UploadState` variant is a COMPILE ERROR here until its outgoing edges are
/// defined — the CLAUDE.md exhaustive-match rule, applied to the state machine.
/// `to` is checked with `matches!`, which needs no exhaustiveness. A terminal
/// `from` has no outgoing edge, so a finished / rejected / given-up row can never
/// be walked back into the pipeline by a racing or stale writer. `Hashing` is a
/// legacy staging state retained for rows persisted by older builds.
pub fn allowed(from: &UploadState, to: &UploadState) -> bool {
    use UploadState::*;
    match from {
        // Terminal — no outgoing edge.
        Completed | Rejected | GaveUp => false,
        // Staging.
        QualityChecking => matches!(to, Staged | Rejected),
        Hashing => matches!(to, Staged | Rejected | Failed),
        Staged => matches!(to, Pending),
        // Claimable idle / failed → active pipeline (+ retry re-arm, give-up).
        Pending => matches!(to, Validating | Transcoding | Creating | Uploading | Failed),
        Failed => matches!(
            to,
            Validating | Transcoding | Creating | Uploading | Pending | GaveUp
        ),
        // Intra-pipeline progress (+ failure). Pause is deliberately NOT reachable
        // from these: a hold only makes sense once bytes are actually going out
        // (the `Uploading` arm). QC/validate/transcode/create/verify are short or
        // in-process stages where pausing has no meaning.
        Validating => matches!(to, Transcoding | Creating | Uploading | Failed),
        Transcoding => matches!(to, Creating | Uploading | Failed),
        Creating => matches!(to, Uploading | Rejected | Failed),
        // The ONLY state a manual pause is allowed from — an in-flight upload.
        Uploading => matches!(to, Verifying | Failed | Paused),
        Verifying => matches!(to, Completed | Failed),
        // Manual hold — resumes only by re-entering the pipeline as Pending.
        Paused => matches!(to, Pending),
    }
}

/// The legal predecessors of `to` — the set a guarded transition's
/// `WHERE state IN (...)` clause should accept. Derived from [`allowed`] so the
/// runtime guard and the pure table cannot drift apart.
pub fn predecessors(to: &UploadState) -> Vec<UploadState> {
    UploadState::ALL
        .into_iter()
        .filter(|from| allowed(from, to))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::UploadState::*;

    #[test]
    fn allowed_is_total_and_terminal_never_reverts() {
        for from in UploadState::ALL {
            for to in UploadState::ALL {
                // Total: no panic for any pair.
                let ok = allowed(&from, &to);
                // A terminal `from` has NO outgoing edge.
                if from.is_terminal() {
                    assert!(!ok, "terminal {from:?} must not transition to {to:?}");
                }
                // Completed is reachable only from Verifying.
                if to == Completed {
                    assert_eq!(ok, from == Verifying, "Completed only from Verifying");
                }
            }
        }
    }

    #[test]
    fn no_backward_or_illegal_edges() {
        assert!(!allowed(&Completed, &Pending));
        assert!(!allowed(&GaveUp, &Pending));
        assert!(!allowed(&Rejected, &Pending));
        assert!(!allowed(&Verifying, &Uploading));
        assert!(!allowed(&Uploading, &Creating));
        assert!(!allowed(&Completed, &Completed));
    }

    #[test]
    fn pause_only_from_uploading() {
        // Pause is meaningful only for an in-flight upload.
        assert!(allowed(&Uploading, &Paused));
        assert!(allowed(&Paused, &Pending));
        for from in UploadState::ALL.into_iter().filter(|s| *s != Uploading) {
            assert!(!allowed(&from, &Paused), "{from:?} must not pause");
        }
        for to in UploadState::ALL.into_iter().filter(|s| *s != Pending) {
            assert!(!allowed(&Paused, &to), "Paused must not go to {to:?}");
        }
    }

    #[test]
    fn happy_path_and_retry_edges_are_legal() {
        assert!(allowed(&Staged, &Pending));
        assert!(allowed(&Pending, &Uploading));
        assert!(allowed(&Pending, &Creating));
        assert!(allowed(&Uploading, &Verifying));
        assert!(allowed(&Verifying, &Completed));
        assert!(allowed(&Uploading, &Failed));
        assert!(allowed(&Failed, &Pending));
        assert!(allowed(&Failed, &GaveUp));
    }

    #[test]
    fn predecessors_derives_from_allowed() {
        let preds = predecessors(&Uploading);
        for p in &preds {
            assert!(allowed(p, &Uploading));
        }
        assert!(preds.contains(&Pending));
        assert!(preds.contains(&Creating));
        assert_eq!(predecessors(&Completed), vec![Verifying]);
        assert_eq!(predecessors(&Paused), vec![Uploading]);
    }

    #[test]
    fn all_covers_every_variant() {
        for s in UploadState::ALL {
            assert_eq!(UploadState::parse(s.as_str()), s);
        }
        let labels: std::collections::HashSet<&str> =
            UploadState::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            labels.len(),
            UploadState::ALL.len(),
            "duplicate label in ALL"
        );
    }
}
