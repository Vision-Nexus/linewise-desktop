# Linewise Desktop

Cross-platform desktop client for [Linewise](https://app.linewise.io) — data desensitization, resumable uploads, video validation, and action camera integration.

Built with **Rust** + **Dioxus**.

![Rust](https://img.shields.io/badge/Rust-2024-orange?logo=rust)
![Dioxus](https://img.shields.io/badge/Dioxus-0.7-blue)
![SQLite](https://img.shields.io/badge/SQLite-sqlx-green)
![License](https://img.shields.io/badge/License-MIT-yellow)

## Features

- **Two-step upload** — select/review files, then confirm. No accidental uploads.
- **Resumable chunked upload** — 32MB chunks, auto-resumes on network recovery. Survives wifi drops, laptop sleep, app restart.
- **Cloud-agnostic storage** — GCS (resumable) and S3-compatible (multipart) backends.
- **Data desensitization** — strips GPS, device info, timestamps from videos/images via ffmpeg before upload.
- **Video validation** — advisory fps/bitrate/resolution checks with tolerance ranges and camera settings guide.
- **Duplicate detection** — BLAKE3 file hashing with SQLite lookup.
- **System tray** — runs in background, hides on close, tray menu to show/quit.
- **Dark/light theme** — follows system appearance automatically.
- **File watcher** — monitor folders for new files, auto-queue for upload.
- **Multi-tenant** — org → project tree sidebar, Firebase Auth (email + OAuth).

## Screenshot

```
┌──────────┬──────────────────────────────────────┐
│ Linewise │  Upload Queue                        │
│          │                                      │
│ ▾ Org A  │  Ready to Upload                     │
│   Proj 1 │  ┌─────────────────────────────────┐ │
│   Proj 2 │  │ video.mp4  120MB     [Remove]   │ │
│          │  └─────────────────────────────────┘ │
│ ▾ Org B  │           [Upload 1 file]            │
│   Proj 1 │                                      │
│          │  Uploading                            │
│          │  ┌─────────────────────────────────┐ │
│          │  │ demo.mp4   ████████░░ 72% [Pause]│ │
│          │  └─────────────────────────────────┘ │
│          │                                      │
│          │  History                              │
│          │  ┌─────────────────────────────────┐ │
│ user@co  │  │ old.mp4  COMPLETED      [Clear] │ │
│ [Sign Out]│  └─────────────────────────────────┘ │
└──────────┴──────────────────────────────────────┘
```

## Quick Start

### Prerequisites

- [Rust](https://rustup.rs/) (2024 edition)
- [ffmpeg](https://ffmpeg.org/) + ffprobe in PATH (for video validation & metadata stripping)

### Build & Run

```bash
cd linewise-desktop

# Dev
cargo run

# Release
cargo build --release
./target/release/lw-app
```

### Configuration

Config is auto-created at:
- **macOS**: `~/Library/Application Support/linewise-desktop/config.toml`
- **Linux**: `~/.config/linewise-desktop/config.toml`
- **Windows**: `%APPDATA%/linewise-desktop/config.toml`

```toml
[server]
environment = "dev"  # dev | testing | production

[upload]
auto_clean = true
chunk_size_mb = 32
max_concurrent_uploads = 4

[desensitization]
strip_metadata = true
```

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for detailed module diagrams (Mermaid), data flow, and resume logic.

### Workspace

```
linewise-desktop/
├── crates/
│   ├── lw-core/    # Business logic (auth, upload, storage, DB, video)
│   └── lw-app/     # Dioxus desktop UI (sidebar, upload queue, login)
├── ARCHITECTURE.md  # Module diagrams
├── CLAUDE.md        # AI coding guidelines
└── Cargo.toml       # Workspace root
```

### Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| **Rust + Dioxus** | Native performance, single binary, cross-platform |
| **sqlx with query! macros** | Compile-time SQL checking, async |
| **Enum storage backend** | GCS + S3 without dyn trait overhead |
| **Two-step upload** | Prevent accidental uploads of wrong files |
| **32MB chunks** | Balance between request overhead and resume granularity |
| **CSS variables for theming** | System dark/light mode via `prefers-color-scheme` |
| **Scala API as source of truth** | DTOs mirror backend case classes, not frontend TS |

## Development

```bash
cargo check           # Type-check
cargo clippy          # Lint (must pass clean)
cargo fmt -- --check  # Format check
cargo test            # Run tests
```

### Environment

```bash
# .env (for sqlx compile-time checking)
DATABASE_URL=sqlite:linewise.db
```

## License

MIT
