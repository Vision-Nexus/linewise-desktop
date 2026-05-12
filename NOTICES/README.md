# Bundled third-party licence texts

This directory contains the full text of every licence referenced by
[THIRD_PARTY_LICENSES.md](../THIRD_PARTY_LICENSES.md) in the repository
root. Both files are shipped inside the installers produced by
Linewise Desktop's CI so downstream users always have the full text
available without a network round-trip.

- [`GPL-2.0.txt`](GPL-2.0.txt) — applies to FFmpeg (when built with
  `--enable-gpl`), libpostproc, x264, and x265.
- [`LGPL-2.1.txt`](LGPL-2.1.txt) — applies to the baseline FFmpeg
  core libraries (`libavcodec`, `libavformat`, `libavutil`,
  `libswscale`, `libswresample`, `libavfilter`) when built without
  `--enable-gpl`.

x264 and x265 both ship under GPL-2.0 verbatim — their upstream
`COPYING` files are identical to `GPL-2.0.txt` here, so no separate
copy is kept.
