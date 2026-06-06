# Architecture — Linewise Desktop

## System Overview

```mermaid
graph TB
    subgraph "lw-app (Dioxus 0.7 Desktop)"
        direction TB
        App["App<br/>System Tray · Session Restore<br/>Dark/Light Theme · Tab Bar"]
        Sidebar["Sidebar<br/>Org → Project Tree"]
        Upload["Upload Queue<br/>Stage → Confirm → Progress<br/>DnD · Retry · Pause"]
        Login["Login<br/>Firebase Email/Password<br/>Google · Microsoft"]
        State["AppState (Signals)<br/>user_info · selected_tenant/project<br/>upload_tasks · auth_token"]
        Core["CoreServices (Arc)"]

        App --> Sidebar
        App --> Upload
        App --> Login
        App --> ChatUI
        Sidebar --> State
        Upload --> State
        Login --> State
        State --> Core
    end

    subgraph "lw-chat (Chat Component)"
        direction TB
        ChatUI["ChatPanel<br/>SSE streaming · bubbles<br/>tool call cards"]
        MD["Markdown<br/>pulldown-cmark → RSX<br/>portable (no innerHTML)"]
        SSE["SSE Client<br/>reqwest byte stream<br/>parse data: lines"]
        ChatTypes["Types<br/>ChatEvent · ChatMessage<br/>ChatRequest · ChatConfig"]

        ChatUI --> MD
        ChatUI --> SSE
        ChatUI --> ChatTypes
    end

    subgraph "lw-core (Business Logic)"
        direction TB
        Auth["Auth<br/>Firebase REST API<br/>Token Refresh · Keyring"]
        API["API Client<br/>whoami · list_projects<br/>create_doc · upload-url · verify"]
        Engine["Upload Engine<br/>stage → confirm → process<br/>resume · auto-retry<br/>4 concurrent (semaphore)"]
        Storage["Storage Backend<br/>GCS (resumable POST)<br/>S3 (multipart)"]
        DB["Database<br/>SQLite (sqlx query!)<br/>upload_queue · file_hashes"]
        Video["Video Validate<br/>ffprobe: fps 20-40<br/>bitrate 10-35Mbps<br/>advisory warnings"]
        Watcher["File Watcher<br/>notify 8.x<br/>per-folder · MIME filter"]
        Dedup["Dedup<br/>BLAKE3 hash<br/>SQLite lookup"]
        Config["Config<br/>TOML · environment<br/>upload · transcode"]

        Engine --> Auth
        Engine --> API
        Engine --> Storage
        Engine --> DB
        Engine --> Video
        Engine --> Dedup
        Watcher --> Engine
    end

    Core --> Auth
    Core --> API
    Core --> Engine
    Core --> DB

    API -->|HTTPS| Backend["Linewise API<br/>(Scala/http4s)"]
    SSE -->|"SSE POST"| Backend
    Storage -->|"Resumable PUT"| GCS["Google Cloud Storage"]
    Storage -->|"Multipart PUT"| S3["S3-Compatible<br/>(AWS/Alibaba/Tencent)"]
    Auth -->|REST| Firebase["Firebase Auth"]
```

## Upload Flow

```mermaid
stateDiagram-v2
    [*] --> Staged : Add Files / DnD
    Staged --> Staged : Remove file
    Staged --> Pending : Confirm Upload

    Pending --> Validating : Start processing
    Validating --> Creating : ffprobe check (advisory)
    Creating --> Uploading : POST create document

    Uploading --> Uploading : Chunk uploaded (progress)
    Uploading --> Paused : User pauses
    Uploading --> Failed : Network error / timeout

    Paused --> Uploading : User resumes
    Paused --> [*] : User removes

    Uploading --> Verifying : All chunks sent
    Verifying --> Completed : gcsUri confirmed
    Verifying --> Failed : Verification timeout

    Failed --> Pending : User retries
    Failed --> Pending : Auto-retry (network recovery)
    Failed --> [*] : User removes

    Completed --> [*] : User clears

    note right of Uploading
        32MB chunks
        5 retries per chunk
        Exponential backoff
        4 concurrent files
    end note

    note right of Failed
        Auto-retry every 30s
        when network recovers
        Max 10 attempts
    end note
```

## Resume Logic

```mermaid
flowchart TD
    Start([App Start / Retry]) --> CheckHash{Has hash?}
    CheckHash -->|No| Hash[BLAKE3 hash file]
    CheckHash -->|Yes| SkipHash[Skip dedup]
    Hash --> DedupCheck{Duplicate?}
    DedupCheck -->|Yes| Fail[FAILED: Duplicate]
    DedupCheck -->|No| CheckDoc
    SkipHash --> CheckDoc

    CheckDoc{Has document_id?}
    CheckDoc -->|No| CreateDoc[POST create document]
    CheckDoc -->|Yes| SkipCreate["Skip create<br/>(reuse existing)"]
    CreateDoc --> CheckSession
    SkipCreate --> CheckSession

    CheckSession{Has session_id?}
    CheckSession -->|No| InitSession["Get signed URL<br/>Initiate resumable session"]
    CheckSession -->|Yes| QueryProgress["Query GCS for<br/>bytes received"]

    QueryProgress --> |Success| Resume["Resume from<br/>byte N"]
    QueryProgress --> |"Session expired"| InitSession

    InitSession --> Upload["Chunked upload<br/>(from byte 0 or N)"]
    Resume --> Upload

    Upload --> Verify["Poll until<br/>gcsUri set"]
    Verify --> Done([COMPLETED])
```

## Module Structure

```mermaid
graph LR
    subgraph "lw-app"
        A1[app.rs]
        A2[sidebar.rs]
        A3[upload_queue.rs]
        A4[login.rs]
        A5[state.rs]
        A6[styles.rs]
        A7[tenant_select.rs]
        A8[project_select.rs]
    end

    subgraph "lw-chat"
        H1[chat.rs]
        H2[markdown.rs]
        H3[client.rs]
        H4[types.rs]
        H5[error.rs]
        H6[styles.rs]
    end

    subgraph "lw-core"
        C1[upload.rs]
        C2[storage.rs]
        C3[api_client.rs]
        C4[auth.rs]
        C5[db.rs]
        C7[video.rs]
        C8[dedup.rs]
        C9[watcher.rs]
        C10[config.rs]
        C11[models.rs]
        C12[error.rs]
    end

    A1 --> H1
    H1 --> H2
    H1 --> H3
    H3 --> H4

    A5 --> C1
    A5 --> C3
    A5 --> C4
    A5 --> C5
    A5 --> C2

    C1 --> C2
    C1 --> C3
    C1 --> C5
    C1 --> C7
    C1 --> C8
    C3 --> C4
    C8 --> C5

    style A1 fill:#dbeafe
    style A2 fill:#dbeafe
    style A3 fill:#dbeafe
    style A4 fill:#dbeafe
    style A5 fill:#bfdbfe
    style A6 fill:#dbeafe
    style H1 fill:#e0e7ff
    style H2 fill:#e0e7ff
    style H3 fill:#e0e7ff
    style C1 fill:#fef3c7
    style C2 fill:#fef3c7
    style C5 fill:#d1fae5
```

## Module Descriptions

### lw-app (Desktop UI — Dioxus 0.7)

| Module | File | Purpose |
|--------|------|---------|
| **App** | `app.rs` | Root component, system tray, session restore, global CSS with dark/light theme, Upload/Chat tab bar |
| **Sidebar** | `components/sidebar.rs` | Two-level org→project tree, expand/collapse, project selection |
| **Upload Queue** | `components/upload_queue.rs` | Two-step upload (stage→confirm), DnD, progress, history, retry/pause/resume/remove |
| **Login** | `components/login.rs` | Email/password login, OAuth buttons (Google, Microsoft) |
| **Tenant Select** | `components/tenant_select.rs` | Organization dropdown (used in sidebar) |
| **Project Select** | `components/project_select.rs` | Project dropdown (used in sidebar) |
| **State** | `state.rs` | `AppState` (Dioxus Signals) + `CoreServices` (Arc shared services) |
| **Styles** | `styles.rs` | Fixed-px layout constants, CSS variable button/input/select styles |

### lw-chat (Chat Component — self-contained)

| Module | File | Purpose |
|--------|------|---------|
| **ChatPanel** | `chat.rs` | Main chat UI: message list, streaming bubbles, tool call cards, input bar. Configurable via `ChatConfig` |
| **Markdown** | `markdown.rs` | Portable pulldown-cmark → RSX renderer. Intermediate `MdNode` tree, no `dangerous_inner_html`. Handles all CommonMark + GFM extensions |
| **SSE Client** | `client.rs` | Streaming HTTP client: POST to chat/completions, parse SSE `data:` lines into `ChatEvent` stream. Error-tolerant (yields parse errors, doesn't stop) |
| **Types** | `types.rs` | `ChatMessage`, `ChatRole`, `ChatEvent` (6 variants: text_delta, thinking_delta, tool_call_start/delta/result, done), `ChatRequest`, `ChatContext`, `ChatMode` |
| **Error** | `error.rs` | `ChatError`: Network, Api, Parse |
| **Styles** | `styles.rs` | `CHAT_CSS` constant: chat panel + markdown content CSS with CSS variable tokens |

### lw-core (Business Logic — zero UI deps)

| Module | File | Purpose |
|--------|------|---------|
| **Auth** | `auth.rs` | Firebase Auth REST API: email sign-in, token refresh (50min), OS keychain storage |
| **API Client** | `api_client.rs` | Linewise backend client: whoami, list_projects, create_document, upload-url, verify |
| **Upload Engine** | `upload.rs` | Orchestrates: stage → confirm → hash → validate → transcode → create → upload → verify. Resumable with auto-retry on network recovery |
| **Storage** | `storage.rs` | Cloud-agnostic enum: `GcsBackend` (resumable POST) + `S3Backend` (multipart). Per-chunk retry with exponential backoff |
| **Database** | `db.rs` | SQLite via sqlx with `query!` macros. Tables: `upload_queue`, `file_hashes`. Async pool |
| **Video** | `video.rs` | ffprobe validation: fps 20-40, bitrate 10-35Mbps, resolution ≥720p. Advisory warnings + camera guide link |
| **Dedup** | `dedup.rs` | BLAKE3 file hashing → SQLite `file_hashes` lookup |
| **Watcher** | `watcher.rs` | `notify` 8.x file system watcher: per-folder with tenant/project mapping, MIME filter, 2s debounce |
| **Config** | `config.rs` | TOML config: server environment, upload prefs, transcode, camera detection, watch folders |
| **Models** | `models.rs` | Domain types mirroring Scala backend DTOs (source of truth: linewise-api) |
| **Error** | `error.rs` | ADT error enums: `AuthError`, `UploadError`, `VideoValidationError`, `DbError`, `ConfigError`, `AppError` |

## Tech Stack

| Layer | Technology |
|-------|-----------|
| UI Framework | Dioxus 0.7 (desktop webview) |
| HTTP | reqwest 0.13 (rustls, 300s timeout) |
| Database | SQLite via sqlx 0.8 (`query!` macros) |
| File Watching | notify 8.x + notify-debouncer-mini |
| File Dialog | rfd 0.17 |
| Video Probing | ffprobe (std::process::Command) |
| File Hashing | BLAKE3 |
| Credentials | keyring 3.x (OS keychain) |
| System Tray | tray-icon (via Dioxus desktop) |
| Theming | CSS variables + `@media (prefers-color-scheme: dark)` |
| Markdown | pulldown-cmark 0.12 (portable RSX renderer) |
| Streaming | async-stream + tokio-stream (SSE parsing) |

## Configuration

```toml
# ~/Library/Application Support/linewise-desktop/config.toml

[server]
environment = "dev"  # dev | testing | production

[upload]
auto_clean = true
chunk_size_mb = 32
max_concurrent_uploads = 4

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
| Chat completions | POST | `/api/org/{tenant}/chat/completions` (SSE stream) |
