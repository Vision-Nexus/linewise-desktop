# Third-Party Licenses

Linewise Desktop is distributed under the GNU General Public License
v2.0 or later (see [LICENSE](LICENSE)). The installers produced by this
project additionally bundle several third-party components — primarily
the FFmpeg libraries and their GPL-licensed companions — that are
subject to their own licences. This document lists every such component
together with the upstream source for the exact version shipped, so
users can exercise the GPL's right to receive the corresponding source
for each binary in the distribution.

The source links below are authoritative for three years following any
binary release of Linewise Desktop. If a link becomes unreachable,
please contact the project maintainers through the GitHub repository
issue tracker and we will provide the corresponding source by other
means.

## Components

| Component | Platform | Version | Licence | Source |
|---|---|---|---|---|
| FFmpeg | macOS | 8.1 (Homebrew bottle) | LGPL-2.1-or-later; GPL-2.0-or-later when built with `--enable-gpl` | https://ffmpeg.org/releases/ffmpeg-8.1.tar.xz |
| FFmpeg | Windows | n7.1 (BtbN "gpl-shared") | GPL-2.0-or-later | Source archive linked from https://github.com/BtbN/FFmpeg-Builds/releases |
| FFmpeg | Linux | 4.4.2 (Ubuntu 22.04) | GPL-2.0-or-later | `apt-get source ffmpeg` on an Ubuntu 22.04 system, or https://launchpad.net/ubuntu/+source/ffmpeg/7:4.4.2-0ubuntu0.22.04.1 |
| libpostproc | all (ships with FFmpeg) | Same as FFmpeg | GPL-2.0-or-later | Same tarball as the parent FFmpeg |
| x264 | macOS (Homebrew), Linux (Ubuntu `libx264-163`) | Homebrew current / Ubuntu `2:0.163.3060+git5db6aa6-2build1` | GPL-2.0-or-later | https://www.videolan.org/developers/x264.html |
| x265 | macOS (Homebrew) | Homebrew current | GPL-2.0-or-later | https://bitbucket.org/multicoreware/x265_git |
| ExifTool | macOS, Linux (Perl script + `lib/`); Windows (standalone exe) | Per `EXIFTOOL_DIST` (e.g. 13.55) | Perl licence (Artistic-1.0-Perl OR GPL-1.0-or-later) | https://exiftool.org / https://github.com/exiftool/exiftool |

## Licence texts

Full text of each licence referenced above is shipped alongside this
document in the [`NOTICES/`](NOTICES/) directory and inside every
installer. The files are:

- `NOTICES/GPL-2.0.txt` — GNU General Public License v2.0 (FFmpeg with
  `--enable-gpl`, x264, x265, libpostproc).
- `NOTICES/LGPL-2.1.txt` — GNU Lesser General Public License v2.1
  (baseline FFmpeg core libraries).

## Rust crates

The Rust code that links into Linewise Desktop depends on a large
number of crates through Cargo. Their licences are recorded in each
crate's `Cargo.toml` under the `license` field and in `Cargo.lock`. A
full machine-generated list can be produced by running
`cargo license --json` against the workspace root. Every crate in the
current dependency tree is either permissively licensed (MIT, Apache
2.0, BSD) or GPL-compatible, and their presence imposes no obligation
beyond attribution.

## Re-bundling

If you are redistributing Linewise Desktop, you must ship this file
and the `NOTICES/` directory with the binary. The bundled installers
produced by this project's CI already do so under
`Contents/Resources/licenses/` on macOS, `/usr/share/doc/linewise-desktop/`
on Linux, and `%ProgramFiles%\Linewise Desktop\licenses\` on Windows.
