# Linewise Desktop

Cross-platform desktop client for Linewise — handles resumable uploads, video validation, and action camera integration. Built with Rust + Dioxus.

**Docs**: [README.md](README.md) | [ARCHITECTURE.md](ARCHITECTURE.md) (Mermaid diagrams)

## Contract

- Core business logic lives in `crates/lw-core/` — zero UI dependencies
- Desktop UI lives in `crates/lw-app/` — thin Dioxus shell over lw-core
- Typed errors via `thiserror` ADT enums in `crates/lw-core/src/error.rs` — add variants as needed
- **linewise-api (Scala) is the single source of truth for all API types.** When defining Rust DTOs in `models.rs`, always read the case classes from `linewise-api/src/` — never guess from frontend TypeScript types. The frontend may transform/rename fields.
- Upload flow must match the web frontend protocol: create doc → get signed URL → PUT to GCS → verify
- All state passed explicitly through function parameters — **no `thread_local!` or `std::thread_local`**
- **Local `let mut` is permitted.** Prefer immutable bindings, but `let mut` inside a function body is fine for loop control and local accumulation. Mutable state must never escape the function — don't return `&mut`, don't store in struct fields, don't pass as `&mut` to other functions. If mutation needs to be shared, use explicit shared-ownership types (`Arc<RwLock<T>>`, Dioxus `Signal`).
- **Database: always use `sqlx` (with SQLite) over `rusqlite`.** sqlx provides async access, compile-time query checking, and migration tooling. Use `sqlx::sqlite::SqlitePool` for connection pooling. **Always use `sqlx::query!` macro** (compile-time checked) instead of `sqlx::query()` function (runtime string). This catches SQL errors at compile time.
- Allowed external crates: see `Cargo.toml` workspace dependencies. Do NOT add new crates without justification.

## Build & Test

```bash
cd linewise-desktop
cargo check                    # Type-check
cargo build                    # Debug build
cargo build --release          # Release build
cargo test                     # Run tests
cargo clippy -- -D warnings    # Lint (must pass clean)
cargo fmt -- --check           # Format check
```

## Architecture

### Workspace Layout

```
crates/
├── lw-core/    # Business logic library (auth, upload engine, API client, DB, video validation)
├── lw-app/     # Dioxus desktop application (UI components, state, tray)
└── lw-cli/     # (future) Headless CLI for scripted uploads
```

### Key Modules (lw-core)

| Module | Purpose |
|--------|---------|
| `auth.rs` | Firebase Auth REST API (email/password, OAuth, token refresh) |
| `api_client.rs` | Linewise API client (documents, upload URLs, verification) |
| `upload.rs` | Upload engine (queue, dedup, validation, upload, verify, auto-clean) |
| `db.rs` | SQLite via rusqlite (upload queue persistence, file hash dedup) |
| `video.rs` | ffprobe-based video parameter validation (30fps/1080p/30Mbps targets) |
| `dedup.rs` | BLAKE3 file hashing for duplicate detection |
| `config.rs` | TOML configuration (server env, upload prefs, transcode, watch folders) |
| `models.rs` | Domain types mirroring backend entities |
| `error.rs` | Typed error ADT enums (AuthError, UploadError, VideoValidationError) |

### Communication Pattern

`lw-core` ↔ `lw-app` via:
- **Core → UI**: `tokio::sync::mpsc::UnboundedSender<UploadEvent>` for async events
- **UI → Core**: Direct async function calls on `Arc<UploadEngine>`, `Arc<AuthService>`, etc.

### API Integration

All endpoints follow `/api/org/{tenant}/...` pattern. Bearer token from Firebase Auth.

| Action | Endpoint |
|--------|----------|
| Who am I | `GET /api/users/whoami` |
| List projects | `GET /api/org/{tenant}/projects` |
| Create document | `POST /api/org/{tenant}/projects/{pid}/documents` |
| Get upload URL | `POST .../documents/{did}/upload-url` |
| Verify upload | `GET .../documents/{did}` |

### Reference Files (web frontend)

- `linewise-frontend/src/data/mutations.ts` — upload flow functions
- `linewise-frontend/src/data/entities.ts` — TypeScript types to mirror
- `linewise-frontend/src/lib/firebase.ts` — Firebase config keys

## Type Safety

- **No `.unwrap()`.** Use `.expect("reason")` for trusted invariants, `?` for untrusted paths.
- **No silent error swallowing.** No `.unwrap_or_default()`, `.ok()` to discard errors.
- **Structured error types** via `thiserror`. Each variant carries domain-specific fields. No `Other(String)` catch-all variants.

  ```rust
  // Bad — stringly typed
  Err(AppError::Config("not found".to_string()))

  // Good — structured variant
  Err(AuthError::InvalidCredentials)
  ```

- **Enums over booleans** for behavioral flags.
- **Exhaustive `match` — no `_ =>` catch-all.** When you add a new enum variant, the compiler must flag every match site.

  ```rust
  // Bad — silently ignores new variants
  match state {
      UploadState::Pending => ...,
      UploadState::Uploading => ...,
      _ => {},
  }

  // Good — compiler forces handling new variants
  match state {
      UploadState::Pending => ...,
      UploadState::Validating => ...,
      UploadState::Transcoding => ...,
      UploadState::Creating => ...,
      UploadState::Uploading => ...,
      UploadState::Verifying => ...,
      UploadState::Completed => ...,
      UploadState::Failed => ...,
      UploadState::Paused => ...,
  }
  ```

- **Wrap values that cross subsystem boundaries.** If a `String` means different things in different contexts (tenant ID vs document ID vs file path), it needs a newtype. If a `bool` parameter controls behavior, it needs an enum.

## Code Style

### Flat Control Flow

- **Prefer early returns and `?` over deeply nested `match`.**
- **Max nesting: 3 levels** (enforced by clippy). Use `let else`, early `return`, `?`, and extracted helpers to reduce brace depth.

  ```rust
  // Bad — deep nesting
  match result {
      Ok(doc) => {
          if doc.gcs_uri.is_some() {
              for task in tasks {
                  if task.state == UploadState::Pending {
                      process(task);
                  }
              }
          }
      }
      Err(_) => {}
  }

  // Good — flat with early returns
  let doc = result?;
  let Some(_uri) = doc.gcs_uri else { return Ok(()); };
  tasks.iter()
      .filter(|t| t.state == UploadState::Pending)
      .for_each(process);
  ```

- **Match arms with >3 lines dispatch to helpers.**

### Functional Style (preferred, not mandatory)

Prefer functional patterns when they make the code clearer. Use imperative style when it's simpler.

- **Prefer iterator pipelines for collection transforms.**
- **Prefer `collect::<Result<Vec<_>, _>>()?` for fallible transforms.**
- **Prefer folds for recursive data construction.**
- **Prefer `split_first()` and slice patterns over indexing.**
- **Prefer declarative matching over flag variables.**

## Refactoring Philosophy

- **Expanding blast radius to reduce tech debt is encouraged.** If adding a field to an enum touches 15 match sites, do it.
- **Fix violations in touched files.** When modifying a file, fix issues in that file — don't defer.
- **Proactive refactoring over workarounds.** If existing code doesn't accommodate a new feature cleanly, refactor the existing code rather than hacking around it.

## Structural Limits

| Constraint | Limit |
|---|---|
| Function length | 150 lines |
| Nesting depth | 3 levels |
| Module file lines | 500 |

## Lint Quick Reference

Enforced via `cargo clippy -- -D warnings` and `clippy.toml`. Hard errors:

| Denied | Use instead |
|---|---|
| `.unwrap()` | `.expect("reason")`, `?`, `.ok_or()` |
| `Result<T, ()>` | Meaningful error type |
| `if !cond { panic!() }` | `assert!(cond, ...)` |
| `thread_local!` | Pass state via parameters |

### Style (auto-fixable)

| Flagged pattern | Use instead |
|---|---|
| `.filter().map()` | `.filter_map()` |
| `.find().map()` | `.find_map()` |
| `.map().flatten()` | `.flat_map()` |
| `for i in 0..v.len()` | `for item in &v` or `.enumerate()` |
| `for x in v.iter()` | `for x in &v` |
| `format!("{}", x)` | `format!("{x}")` |
| `let x = if let Some(v) = y { v } else { return }` | `let Some(x) = y else { return }` |
