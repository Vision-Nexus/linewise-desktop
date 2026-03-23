# Architecture — Linewise Desktop

## Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                        lw-app (Dioxus)                         │
│  ┌──────────┐ ┌──────────────┐ ┌────────────┐ ┌─────────────┐ │
│  │  Sidebar  │ │ Upload Queue │ │   Login    │ │   Styles    │ │
│  │  (tree)   │ │ (2-step +    │ │ (Firebase  │ │ (CSS vars,  │ │
│  │  org→proj │ │  history)    │ │  REST)     │ │  dark/light)│ │
│  └──────┬───┘ └──────┬───────┘ └─────┬──────┘ └─────────────┘ │
│         │            │               │                         │
│  ┌──────┴────────────┴───────────────┴──────────────────────┐  │
│  │                    AppState (Signals)                     │  │
│  │  is_authenticated, user_info, selected_tenant/project,   │  │
│  │  upload_tasks, tenant_projects                           │  │
│  └──────────────────────┬───────────────────────────────────┘  │
│                         │                                      │
│  ┌──────────────────────┴───────────────────────────────────┐  │
│  │               CoreServices (Arc<...>)                     │  │
│  │  auth, api, db, upload_engine, storage, config            │  │
│  └──────────────────────┬───────────────────────────────────┘  │
│         System Tray     │    Window: hide-to-tray              │
└─────────────────────────┼──────────────────────────────────────┘
                          │
┌─────────────────────────┼──────────────────────────────────────┐
│                      lw-core                                   │
│                         │                                      │
│  ┌──────────┐  ┌────────┴────────┐  ┌─────────────────────┐   │
│  │   Auth    │  │  Upload Engine  │  │     API Client      │   │
│  │ Firebase  │  │  stage → confirm│  │  whoami, create doc, │   │
│  │ REST API  │  │  → process →    │  │  upload-url, verify  │   │
│  │ keyring   │  │  resume/retry   │  │  Bearer token auth   │   │
│  └──────────┘  └────────┬────────┘  └─────────────────────┘   │
│                         │                                      │
│  ┌──────────┐  ┌────────┴────────┐  ┌─────────────────────┐   │
│  │ Storage   │  │    Database     │  │   Desensitize       │   │
│  │ Backend   │  │  SQLite (sqlx)  │  │  ffmpeg metadata    │   │
│  │ ┌──────┐  │  │  upload_queue   │  │  strip (video/img)  │   │
│  │ │ GCS  │  │  │  file_hashes    │  └─────────────────────┘   │
│  │ └──────┘  │  │  query! macros  │                            │
│  │ ┌──────┐  │  └─────────────────┘  ┌─────────────────────┐   │
│  │ │  S3  │  │                       │    Video Validate    │   │
│  │ └──────┘  │  ┌─────────────────┐  │  ffprobe: fps,      │   │
│  └──────────┘  │   File Watcher   │  │  bitrate, resolution│   │
│                │  notify (8.x)    │  │  advisory warnings   │   │
│                │  per-folder +    │  │  + camera guide link │   │
│                │  MIME filter     │  └─────────────────────┘   │
│                └─────────────────┘                             │
│                                       ┌─────────────────────┐  │
│  ┌──────────┐  ┌─────────────────┐   │      Dedup          │  │
│  │  Config   │  │    Models       │   │  BLAKE3 hash →      │  │
│  │  TOML     │  │  mirrors Scala  │   │  SQLite lookup      │  │
│  │  env/     │  │  backend DTOs   │   └─────────────────────┘  │
│  │  upload/  │  └─────────────────┘                            │
│  │  camera/  │                                                 │
│  │  desensi- │  ┌─────────────────┐                            │
│  │  tization │  │    Errors       │                            │
│  └──────────┘  │  ADT enums:     │                            │
│                │  Auth, Upload,   │                            │
│                │  Video, Db,     │                            │
│                │  Config, App    │                            │
│                └─────────────────┘                            │
└────────────────────────────────────────────────────────────────┘
```

## Module Descriptions

### lw-app (Desktop UI — Dioxus 0.7)

| Module | File | Purpose |
|--------|------|---------|
| **App** | `app.rs` | Root component, system tray, session restore, global CSS with dark/light theme |
| **Sidebar** | `components/sidebar.rs` | Two-level org→project tree, expand/collapse, project selection |
| **Upload Queue** | `components/upload_queue.rs` | Two-step upload (stage→confirm), DnD, progress, history, retry/pause/resume/remove |
| **Login** | `components/login.rs` | Email/password login, OAuth buttons (Google, Microsoft) |
| **Tenant Select** | `components/tenant_select.rs` | Organization dropdown (used in sidebar) |
| **Project Select** | `components/project_select.rs` | Project dropdown (used in sidebar) |
| **State** | `state.rs` | `AppState` (Dioxus Signals) + `CoreServices` (Arc shared services) |
| **Styles** | `styles.rs` | Fixed-px layout constants, CSS variable button/input/select styles |

### lw-core (Business Logic — zero UI deps)

| Module | File | Purpose |
|--------|------|---------|
| **Auth** | `auth.rs` | Firebase Auth REST API: email sign-in, token refresh (50min), OS keychain storage |
| **API Client** | `api_client.rs` | Linewise backend client: whoami, list_projects, create_document, upload-url, verify |
| **Upload Engine** | `upload.rs` | Orchestrates: stage → confirm → hash → validate → desensitize → create → upload → verify. Resumable — skips completed stages on retry |
| **Storage** | `storage.rs` | Cloud-agnostic enum: `GcsBackend` (resumable POST) + `S3Backend` (multipart). Chunked upload with progress callback |
| **Database** | `db.rs` | SQLite via sqlx with `query!` macros. Tables: `upload_queue`, `file_hashes`. Async pool |
| **Desensitize** | `desensitize.rs` | ffmpeg metadata stripping: `-map_metadata -1 -c copy`. Video + image support |
| **Video** | `video.rs` | ffprobe validation: fps 20-40, bitrate 10-35Mbps, resolution ≥720p. Advisory warnings + camera guide link |
| **Dedup** | `dedup.rs` | BLAKE3 file hashing → SQLite `file_hashes` lookup |
| **Watcher** | `watcher.rs` | `notify` 8.x file system watcher: per-folder with tenant/project mapping, MIME filter, 2s debounce |
| **Config** | `config.rs` | TOML config: server environment, upload prefs, desensitization, camera detection, watch folders |
| **Models** | `models.rs` | Domain types mirroring Scala backend DTOs (source of truth: linewise-api) |
| **Error** | `error.rs` | ADT error enums: `AuthError`, `UploadError`, `VideoValidationError`, `DbError`, `ConfigError`, `AppError` |

## Data Flow

### Upload Flow (two-step)

```
User selects files (button or DnD)
  │
  ▼
Stage files → SQLite (state: STAGED)
  │
  ▼ user clicks "Upload N files"
  │
Confirm staged → state: PENDING
  │
  ├─► BLAKE3 hash → dedup check (SQLite file_hashes)
  │
  ├─► ffprobe validation (advisory warnings)
  │
  ├─► ffmpeg metadata strip (if enabled)
  │
  ├─► POST /api/.../documents → create document (if no document_id yet)
  │
  ├─► POST /api/.../upload-url?resumable=true → signed URL
  │
  ├─► POST signed URL + x-goog-resumable:start → session URI (if no session_id yet)
  │
  ├─► PUT chunks (8MB) to session URI → progress events → UI
  │   (resumes from last confirmed byte on retry)
  │
  ├─► GET /api/.../documents/{id} → poll until gcsUri set
  │
  └─► state: COMPLETED, record hash, auto-clean
```

### Resume on Restart

```
App starts
  │
  ├─► reset_stale_uploads(): UPLOADING/CREATING/etc → FAILED
  │
  ├─► Load all uploads from SQLite → UI
  │
  └─► User clicks "Retry" on failed task
        │
        ├─► Has document_id? → skip create, reuse it
        ├─► Has session_id? → query GCS for progress → resume from byte N
        └─► Session expired? → get new URL, re-initiate, upload from 0
```

## Tech Stack

| Layer | Technology |
|-------|-----------|
| UI Framework | Dioxus 0.7 (desktop webview) |
| HTTP | reqwest 0.13 (rustls) |
| Database | SQLite via sqlx 0.8 (`query!` macros) |
| File Watching | notify 8.x + notify-debouncer-mini |
| File Dialog | rfd 0.17 |
| Video Probing | ffprobe (std::process::Command) |
| Metadata Strip | ffmpeg (std::process::Command) |
| File Hashing | BLAKE3 |
| Credentials | keyring 3.x (OS keychain) |
| System Tray | tray-icon (via Dioxus desktop) |
| Theming | CSS variables + `@media (prefers-color-scheme: dark)` |

## Configuration

```toml
# ~/Library/Application Support/linewise-desktop/config.toml

[server]
environment = "dev"  # dev | testing | production

[upload]
auto_clean = true
chunk_size_mb = 8
max_concurrent_uploads = 3

[desensitization]
strip_metadata = true
blur_faces = false              # future: ONNX model
processing_mode = "local"       # local | remote

[camera]
auto_detect = true              # future: USB detection

[[watch_folders]]               # future: file watcher config
path = "/Users/me/Videos"
tenant_id = "acme"
project_id = "proj-123"
file_filter = ["video/*"]
```

## API Endpoints Used

| Action | Method | Endpoint |
|--------|--------|----------|
| Who am I | GET | `/api/users/whoami` |
| List projects | GET | `/api/org/{tenant}/projects` |
| Create document | POST | `/api/org/{tenant}/projects/{pid}/documents` |
| Get upload URL | POST | `.../documents/{did}/upload-url?resumable=true` |
| Get document | GET | `.../documents/{did}` |
| Verify upload | GET | `.../documents/{did}` (poll until gcsUri set) |
