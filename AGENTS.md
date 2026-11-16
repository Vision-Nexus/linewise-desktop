# Linewise Desktop

Cross-platform Linewise client for resumable uploads, video validation, and action-camera ingestion. See [README.md](README.md) and [ARCHITECTURE.md](ARCHITECTURE.md) for the user and system overview.

## Core contracts

- `crates/lw-core/` owns business logic and has no UI dependency; `crates/lw-app/` is the thin Dioxus shell.
- Use `thiserror` domain error enums for core operations. Keep variants specific to the failure and carry useful domain context; do not replace the error model with `Other(String)`.
- `linewise-api` is the source of truth for API DTOs. Read its Scala case classes before changing Rust models; do not copy or infer fields from the web client's transformed TypeScript types.
- Uploads follow the web contract: create the document, request its signed upload URL, PUT the file to GCS, then verify the document.
- The workspace uses SQLx with SQLite, not `rusqlite`. Justify any new external crate in the change.

## State and database rules

Pass state explicitly and do not use `thread_local!` or `std::thread_local`; `clippy.toml` disallows the macro and CI runs Clippy with warnings denied.

Local `let mut` is fine for loop control and local accumulation. Avoid storing shared mutable state in structs; use explicit ownership or synchronization when state must be shared across tasks.

Prefer `sqlx::query!` or `sqlx::query_as!` for static SQL when the offline query cache can check it. Runtime `sqlx::query()` remains appropriate for dynamic SQL or a static statement whose cache entry would add needless upkeep. `crates/lw-core/src/db.rs` has an intentional runtime-query example for that reason.

## Build and release commands

```bash
cargo check
cargo test
cargo clippy -- -D warnings
cargo fmt -- --check
cargo xtask release vX.Y.Z
```

The workspace contains `lw-core`, `lw-app`, and `xtask`; `crates/lw-chat/` is excluded and is not mounted by the app. The release command updates the workspace version, refreshes the lockfile, commits, and tags the release, so use it only for an explicitly requested release.

## API and upload behavior

The desktop client sends Firebase Bearer tokens to the API. Tenant-scoped requests use `/api/org/{tenant}/...`; the who-am-I call is user-scoped at `/api/users/whoami`.

Keep the upload order intact: create the document, request the signed URL, upload bytes directly to GCS, and verify the completed document through the API. Do not send file bytes through the document-creation request.

The signed-URL flow is shared with the web client. Changes to upload metadata, verification, or retry behavior need to remain compatible with the API and browser implementation.

`lw-core` emits upload events to the UI over a Tokio unbounded channel. The UI calls core services through their async APIs; keep UI rendering and platform-specific behavior out of core.

## Persistence and external storage

SQLite persistence is accessed through the SQLx pool. Keep database operations async and use the existing migrations and row models when changing the upload queue.

The upload engine persists queue state and file-hash data so it can resume after restart and avoid duplicate work. Preserve that link when changing queue or deduplication semantics.

Cloud storage uses signed URLs for GCS and supports the configured S3-compatible backends. Keep provider-specific storage behavior behind the storage layer rather than leaking it into UI code.

Video validation uses the existing ffprobe-based checks. If changing limits or accepted formats, keep validation errors typed so the caller can explain the rejected media.

## Domain and failure behavior

Use structured errors for authentication, upload, configuration, storage, and video validation failures. Each error should retain enough context to identify the failed operation without exposing credentials.

## Clippy configuration

`clippy.toml` sets a 200-line function threshold and a nesting threshold of 5 and disallows `std::thread_local`. CI runs `cargo clippy --locked --workspace --all-targets -- -D warnings`.

## References for contract changes

The web upload implementation is in `../linewise-frontend/src/data/mutations.ts`; its entity types are in `../linewise-frontend/src/data/entities.ts`.

The desktop API client, upload engine, and persistent queue live in `crates/lw-core/`. Keep shared API contract changes aligned with `linewise-api` and the web client.

When touching packaging or release behavior, use the `xtask` command rather than hand-editing version metadata or tags.

Keep `crates/lw-chat/` separate from the active app: it is present in the repository but excluded from the Cargo workspace and is not a dependency of `lw-app`.

After changing a SQL query or migration, check whether the checked-in SQLx offline cache must change. Runtime queries are an intentional exception when maintaining a compile-time cache entry is not worthwhile.

## Authentication and configuration

Firebase authentication supports email and password plus Google and Microsoft sign-in. The OAuth flow uses an authorization-code PKCE loopback; keep that native callback flow intact when changing login behavior.

The API client sends the Firebase ID token as a Bearer token. Keep token acquisition and refresh in the auth service rather than duplicating it in the UI or upload engine.

User configuration is TOML and covers server environment, upload preferences, and watched folders. Preserve existing defaults when adding a field so an older configuration remains usable.

## Upload state and storage

The upload queue is persisted in SQLite so queued work and its deduplication data survive an app restart. Changes to queue identity, hashes, or persisted states must keep the database representation and upload engine in agreement.

File deduplication uses BLAKE3 hashes. Keep hashing tied to the current file content; do not treat a path alone as proof that a file is unchanged.

The core upload engine validates and queues files, creates the backend document, transfers the file, verifies completion, and reports progress to the UI through `UploadEvent`.

Keep document creation and file transfer as separate requests. The API creates the document and returns a signed URL; the client sends file bytes directly to the storage provider, then asks the API to verify the result.

The GCS path uses signed URLs. Other configured S3-compatible storage providers stay behind the storage abstraction; do not put provider-specific request code in the Dioxus UI.

`lw-app` listens for core upload events and invokes core async operations. Preserve this direction when changing progress reporting or cancellation so UI state does not become a second upload engine.

## Media validation

Video checks use ffprobe through the core validation path. Keep accepted-media decisions in core so files selected from watched folders and files selected in the UI receive the same checks.

Validation failures use typed errors that the queue and UI can present. Do not replace them with a generic string at the boundary or discard the reason a file was rejected.

## Workspace and release boundaries

`xtask` owns release packaging and version/tag changes. The `cargo xtask release vX.Y.Z` command performs those changes and creates a commit and tag; do not run it for ordinary code edits.

## Upload state transitions

The upload states are the `UploadState` enum in `crates/lw-core/src/models.rs`. The former desensitization stage was removed on purpose because its strip pass also removed capture tags embedded before upload (see `crates/lw-core/src/upload.rs` and migration `0009_retire_desensitizing_state.sql`); do not reintroduce it.

Keep the enum, persisted queue state, restart behavior, and UI progress events aligned when adding or changing a state. A state that is not decoded after restart can strand a queued file.

The core owns state transitions and sends progress through `UploadEvent`. The UI observes those events; it should not invent a competing state machine for the same upload.

Database migrations and SQLx offline data must match any persistent-state change. Prefer the compile-checked query macros when their cache can be maintained; use the documented runtime-query exception when it cannot.

## Packaging and dependencies

`xtask` bundles ffmpeg and exiftool and generates platform icons. Update release packaging when a bundled runtime tool changes; a local installation is not part of a release artifact.

Shared dependencies are declared at workspace level. When a justified crate is added, keep member manifests and the workspace lockfile consistent.

The app shell should keep calling core APIs for file processing rather than growing a second platform-specific upload path.
