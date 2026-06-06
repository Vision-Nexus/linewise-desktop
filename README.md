# Linewise Upload

Cross-platform desktop client for [Linewise](https://app.linewise.io) — resumable uploads, video validation, and action camera integration.

Built with **Rust** + **Dioxus** + **Tailwind CSS v4**.

![Rust](https://img.shields.io/badge/Rust-2024-orange?logo=rust)
![Dioxus](https://img.shields.io/badge/Dioxus-0.7-blue)
![Tailwind](https://img.shields.io/badge/Tailwind_CSS-v4-38bdf8?logo=tailwindcss)
![SQLite](https://img.shields.io/badge/SQLite-sqlx-green)
![License](https://img.shields.io/badge/License-GPLv2--or--later-blue)

## Features

- **Two-step upload** — select/review files, then confirm. No accidental uploads.
- **Resumable chunked upload** — 32MB chunks, auto-resumes on network recovery. Survives wifi drops, laptop sleep, app restart.
- **Cloud-agnostic storage** — GCS (resumable) and S3-compatible (multipart) backends.
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
- [Node.js](https://nodejs.org/) (v20+) — for Tailwind CSS build
- [ffmpeg](https://ffmpeg.org/) + ffprobe in PATH (for video validation & metadata stripping)

### Setup

```bash
cd linewise-desktop

# Install Tailwind CSS and dependencies
npm install
```

### Build & Run

```bash
# Dev (Tailwind CSS is generated automatically during cargo build)
cargo run

# Release
cargo build --release
./target/release/linewise-desktop
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
```

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for detailed module diagrams (Mermaid), data flow, and resume logic.

### Workspace

```
linewise-desktop/
├── crates/
│   ├── lw-core/    # Business logic (auth, upload, storage, DB, video)
│   └── lw-app/     # Dioxus desktop UI (sidebar, upload queue, login)
├── package.json     # Tailwind CSS dev dependency
├── ARCHITECTURE.md  # Module diagrams
├── CLAUDE.md        # AI coding guidelines
└── Cargo.toml       # Workspace root
```

### Tailwind CSS Integration

The login page uses Tailwind CSS v4 with theme tokens matching the web frontend. The build pipeline:

1. `crates/lw-app/input.css` — Tailwind entry point with theme variables
2. `build.rs` runs `npx @tailwindcss/cli` at compile time, scanning `.rs` files for class names
3. Generated CSS is included via `include_str!` into the Dioxus webview

### Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| **Rust + Dioxus** | Native performance, single binary, cross-platform |
| **Tailwind CSS v4** | Consistent styling with web frontend, utility-first |
| **sqlx with query! macros** | Compile-time SQL checking, async |
| **Enum storage backend** | GCS + S3 without dyn trait overhead |
| **Two-step upload** | Prevent accidental uploads of wrong files |
| **32MB chunks** | Balance between request overhead and resume granularity |
| **Scala API as source of truth** | DTOs mirror backend case classes, not frontend TS |

## Development

```bash
npm install             # Install Tailwind CSS (first time only)
cargo check            # Type-check
cargo clippy           # Lint (must pass clean)
cargo fmt -- --check   # Format check
cargo test             # Run tests
```

### Environment

```bash
# .env (for sqlx compile-time checking)
DATABASE_URL=sqlite:linewise.db
```

## License

Linewise Desktop is licensed under the **GNU General Public License v2.0
or later**. See [LICENSE](LICENSE) for the full text.

This binary distribution includes ffmpeg, x264, x265, and libpostproc —
each licensed under the GNU GPL. Full attributions, source links, and
licence texts are in [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md)
and in the [`NOTICES/`](NOTICES/) directory.
