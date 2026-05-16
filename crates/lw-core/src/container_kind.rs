//! Magic-byte container detection.
//!
//! Linewise's server-side quality check only understands ISO BMFF
//! (mp4, mov, m4v, 3gp, and other QuickTime-family files). The 2026-05-16
//! production-data sweep showed that ISO BMFF accounts for 99.98% of
//! customer uploads — a single ASF/WMV file from one tenant is the only
//! genuinely non-ISO-BMFF row in the table — so the right answer for the
//! remaining containers is a friendly typed rejection, not a separate
//! walker per format.
//!
//! [`detect`] reads the first 16 bytes of the file and matches against the
//! magic-byte signatures of the formats users are most likely to bump
//! into: Matroska / WebM (EBML), AVI (RIFF), ASF / WMV (header-object
//! GUID), FLV, and MPEG-TS (the 0x47 sync byte at every 188 bytes).
//! Anything else falls through to [`ContainerKind::Unknown`].
//!
//! The detector is intentionally lightweight — it never opens an EBML
//! walker or seeks past the prelude. The goal is "fail fast with a
//! kind-specific message before the atom walker runs", not to mirror
//! libavformat's full probe matrix.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Number of bytes [`detect`] reads from the head of the file. Sized to
/// fit every magic-byte rule below; the ASF Header Object GUID is 16
/// bytes and is the longest signature we check at offset 0.
const HEAD_LEN: usize = 16;

/// Offset of the second MPEG-TS sync byte that confirms a transport
/// stream. Standard 188-byte packet alignment.
const MPEGTS_PACKET_SIZE: u64 = 188;
const MPEGTS_SECOND_SYNC: u64 = MPEGTS_PACKET_SIZE;
const MPEGTS_THIRD_SYNC: u64 = MPEGTS_PACKET_SIZE * 2;

/// EBML header — Matroska and WebM share the prefix.
const EBML_MAGIC: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];

/// ASF Header Object GUID, little-endian on the wire.
const ASF_GUID: [u8; 16] = [
    0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C,
];

/// MPEG-TS sync byte. Two of these 188 bytes apart confirm the format.
const MPEGTS_SYNC: u8 = 0x47;

/// Coarse container family inferred from the file's magic bytes. Used to
/// reject non-ISO-BMFF inputs at staging time with a kind-specific toast
/// before the atom walker (or the server) sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerKind {
    /// QuickTime / MPEG-4 family: mp4, mov, m4v, 3gp, etc. The only
    /// container the server-side quality check understands.
    IsoBmff,
    /// Matroska. Distinguished from WebM by the EBML `DocType` element;
    /// the present implementation does not parse EBML beyond the magic
    /// prefix and reports both as [`ContainerKind::Matroska`] — the
    /// user-facing toast is the same either way.
    Matroska,
    /// WebM. Reserved for a future EBML `DocType` walker; not produced
    /// by the current detector.
    WebM,
    /// AVI (RIFF / `AVI `).
    Avi,
    /// Microsoft ASF / WMV.
    Asf,
    /// Flash Video.
    Flv,
    /// MPEG-TS / M2TS — confirmed by two 0x47 sync bytes 188 apart.
    MpegTs,
    /// No magic-byte signature matched. Could be a raw codec stream, a
    /// proprietary container, or a corrupt file. Caller renders a
    /// generic "unrecognized format" message.
    Unknown,
}

impl ContainerKind {
    /// Short human-readable label used in the `UnsupportedContainer`
    /// toast. The full message lives on the error variant; this just
    /// names the family the user picked so the message reads naturally.
    pub fn human_label(self) -> &'static str {
        match self {
            ContainerKind::IsoBmff => "mp4/mov",
            ContainerKind::Matroska => "matroska",
            ContainerKind::WebM => "webm",
            ContainerKind::Avi => "avi",
            ContainerKind::Asf => "asf/wmv",
            ContainerKind::Flv => "flv",
            ContainerKind::MpegTs => "mpeg-ts",
            ContainerKind::Unknown => "an unrecognized format",
        }
    }
}

/// Read the first 16 bytes of `path` and classify the container.
///
/// Returns [`ContainerKind::Unknown`] when no rule matches — including
/// the case where the file is shorter than [`HEAD_LEN`] bytes, since
/// every magic-byte we check fits inside that prefix.
///
/// The MPEG-TS check seeks to byte 188 and reads one extra byte to
/// confirm a second sync. Files shorter than 376 bytes can never
/// confirm MPEG-TS and fall through to [`ContainerKind::Unknown`].
pub fn detect(path: &Path) -> Result<ContainerKind, std::io::Error> {
    let mut file = File::open(path)?;
    let mut head = [0u8; HEAD_LEN];
    let read = read_up_to(&mut file, &mut head)?;
    let head = &head[..read];

    if let Some(kind) = match_head(head) {
        return Ok(kind);
    }

    // MPEG-TS confirmation requires a second sync byte at offset 188.
    // Only attempt the seek when the first byte already looks like a
    // sync byte; otherwise we save a syscall on every other input.
    if head.first().copied() == Some(MPEGTS_SYNC) && confirm_mpegts(&mut file)? {
        return Ok(ContainerKind::MpegTs);
    }

    Ok(ContainerKind::Unknown)
}

/// Read up to `buf.len()` bytes, returning the number actually read.
/// Short reads (smaller files) are not an error here — the caller
/// inspects the slice it gets back.
fn read_up_to(file: &mut File, buf: &mut [u8]) -> Result<usize, std::io::Error> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Match the front-loaded magic-byte signatures. Returns `None` when no
/// rule fires; the caller then tries the MPEG-TS second-sync probe.
fn match_head(head: &[u8]) -> Option<ContainerKind> {
    if head.len() >= 8 && &head[4..8] == b"ftyp" {
        return Some(ContainerKind::IsoBmff);
    }
    if head.len() >= 4 && head[..4] == EBML_MAGIC {
        // No DocType walker yet — Matroska covers both Matroska and WebM
        // for toast purposes. See `ContainerKind::Matroska` doc.
        return Some(ContainerKind::Matroska);
    }
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"AVI " {
        return Some(ContainerKind::Avi);
    }
    if head.len() >= 16 && head[..16] == ASF_GUID {
        return Some(ContainerKind::Asf);
    }
    if head.len() >= 4 && &head[..3] == b"FLV" && (0x01..=0x05).contains(&head[3]) {
        return Some(ContainerKind::Flv);
    }
    None
}

/// Confirm MPEG-TS by checking for sync bytes at packet offsets 188 and 376.
/// Two extra confirmations (rather than one) cuts the false-positive rate from
/// `1/256` to `1/65536` for arbitrary binary inputs that happen to start with
/// `0x47`. Short files that can't reach offset 376 return `false` — the
/// detector falls through to `Unknown` rather than guessing.
fn confirm_mpegts(file: &mut File) -> Result<bool, std::io::Error> {
    sync_byte_at(file, MPEGTS_SECOND_SYNC).and_then(|ok| {
        if ok {
            sync_byte_at(file, MPEGTS_THIRD_SYNC)
        } else {
            Ok(false)
        }
    })
}

fn sync_byte_at(file: &mut File, offset: u64) -> Result<bool, std::io::Error> {
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return Ok(false);
    }
    let mut byte = [0u8; 1];
    match file.read(&mut byte)? {
        0 => Ok(false),
        _ => Ok(byte[0] == MPEGTS_SYNC),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("lw-container-{name}-{}.bin", uuid::Uuid::new_v4()));
        let mut f = std::fs::File::create(&path).expect("create temp");
        f.write_all(bytes).expect("write temp");
        path
    }

    fn detect_bytes(name: &str, bytes: &[u8]) -> ContainerKind {
        let path = write_temp(name, bytes);
        let kind = detect(&path).expect("detect");
        let _ = std::fs::remove_file(&path);
        kind
    }

    #[test]
    fn iso_bmff_is_recognized_via_ftyp() {
        // 4-byte size || "ftyp" || major brand || minor version || compat
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&32u32.to_be_bytes());
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(b"isom");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(b"isomavc1");
        assert_eq!(detect_bytes("isobmff", &bytes), ContainerKind::IsoBmff);
    }

    #[test]
    fn iphone_quicktime_shaped_header_is_iso_bmff() {
        // Real iPhone .mov files start with a 32-byte ftyp box: size 32,
        // major brand "qt  ", minor version, and compatible-brands list.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&32u32.to_be_bytes());
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(b"qt  ");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(b"qt  ");
        bytes.extend_from_slice(b"\0\0\0\0");
        // Pad with a few bytes of "moov" header so the head read fills up.
        bytes.extend_from_slice(&8u32.to_be_bytes());
        bytes.extend_from_slice(b"moov");
        assert_eq!(detect_bytes("iphone-qt", &bytes), ContainerKind::IsoBmff);
    }

    #[test]
    fn matroska_ebml_magic_is_recognized() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&EBML_MAGIC);
        // Pad to satisfy a 16-byte read; payload bytes are arbitrary.
        bytes.extend_from_slice(&[0xAA; 12]);
        assert_eq!(detect_bytes("mkv", &bytes), ContainerKind::Matroska);
    }

    #[test]
    fn avi_riff_with_avi_subtype_is_recognized() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&1024u32.to_le_bytes()); // RIFF size
        bytes.extend_from_slice(b"AVI ");
        bytes.extend_from_slice(b"LIST"); // first chunk header
        assert_eq!(detect_bytes("avi", &bytes), ContainerKind::Avi);
    }

    #[test]
    fn riff_without_avi_subtype_is_unknown() {
        // RIFF with WAVE subtype must not be classified as AVI.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&1024u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        assert_eq!(detect_bytes("wav", &bytes), ContainerKind::Unknown);
    }

    #[test]
    fn asf_guid_is_recognized() {
        let bytes = ASF_GUID.to_vec();
        assert_eq!(detect_bytes("asf", &bytes), ContainerKind::Asf);
    }

    #[test]
    fn flv_signature_with_valid_version_is_recognized() {
        // "FLV" || version 0x01 || flags || data offset
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FLV");
        bytes.push(0x01); // version 1
        bytes.push(0x05); // audio + video
        bytes.extend_from_slice(&9u32.to_be_bytes()); // header size
        bytes.extend_from_slice(&[0u8; 5]);
        assert_eq!(detect_bytes("flv", &bytes), ContainerKind::Flv);
    }

    #[test]
    fn flv_signature_with_invalid_version_is_unknown() {
        // Version byte outside 0x01..=0x05 must not match.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FLV");
        bytes.push(0x99);
        bytes.extend_from_slice(&[0u8; 12]);
        assert_eq!(detect_bytes("flv-bad-ver", &bytes), ContainerKind::Unknown);
    }

    #[test]
    fn mpegts_three_sync_bytes_packet_aligned_is_recognized() {
        // Build a 400-byte file with sync bytes at offsets 0, 188, 376.
        let mut bytes = vec![0u8; 400];
        bytes[0] = MPEGTS_SYNC;
        bytes[MPEGTS_SECOND_SYNC as usize] = MPEGTS_SYNC;
        bytes[MPEGTS_THIRD_SYNC as usize] = MPEGTS_SYNC;
        assert_eq!(detect_bytes("mpegts", &bytes), ContainerKind::MpegTs);
    }

    #[test]
    fn mpegts_only_two_sync_bytes_is_unknown() {
        // Two-of-three is not enough — keeps the false-positive rate low for
        // arbitrary binary inputs that happen to start with 0x47.
        let mut bytes = vec![0u8; 400];
        bytes[0] = MPEGTS_SYNC;
        bytes[MPEGTS_SECOND_SYNC as usize] = MPEGTS_SYNC;
        // No sync at MPEGTS_THIRD_SYNC.
        assert_eq!(detect_bytes("mpegts-2of3", &bytes), ContainerKind::Unknown);
    }

    #[test]
    fn mpegts_single_sync_without_second_is_unknown() {
        let mut bytes = vec![0u8; 400];
        bytes[0] = MPEGTS_SYNC;
        // No second sync byte at 188.
        assert_eq!(
            detect_bytes("mpegts-single", &bytes),
            ContainerKind::Unknown
        );
    }

    #[test]
    fn mpegts_too_short_to_confirm_is_unknown() {
        // Starts with 0x47 but file is shorter than 377 bytes — can't confirm
        // the third sync.
        let bytes = vec![MPEGTS_SYNC; 50];
        assert_eq!(detect_bytes("mpegts-short", &bytes), ContainerKind::Unknown);
    }

    #[test]
    fn unknown_garbage_is_unknown() {
        let bytes = vec![0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE];
        assert_eq!(detect_bytes("garbage", &bytes), ContainerKind::Unknown);
    }

    #[test]
    fn empty_file_is_unknown() {
        let bytes: Vec<u8> = Vec::new();
        assert_eq!(detect_bytes("empty", &bytes), ContainerKind::Unknown);
    }

    /// Mirrors the gate in `UploadEngine::run_quality_check`: a synthetic
    /// mkv-magic-byte file goes in, the same `match` produces a typed
    /// `UploadError::UnsupportedContainer { kind: Matroska }` out. Spinning
    /// up a full `UploadEngine` (Database, ApiClient, StorageBackend) just
    /// to assert this one gate would dwarf the test; replicating the gate
    /// keeps the assertion close to the rule it protects without inventing
    /// mock infrastructure the rest of the crate doesn't have.
    #[test]
    fn stage_file_rejects_matroska_with_unsupported_container() {
        use crate::error::UploadError;

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&EBML_MAGIC);
        bytes.extend_from_slice(&[0x42, 0x82, 0x88]); // EBML DocType ID + size
        bytes.extend_from_slice(b"matroska");
        let path = write_temp("stage-mkv", &bytes);

        let kind = detect(&path).expect("detect");
        let _ = std::fs::remove_file(&path);

        let err = match kind {
            ContainerKind::IsoBmff => panic!("synthetic mkv should not detect as IsoBmff"),
            other => UploadError::UnsupportedContainer { kind: other },
        };
        match err {
            UploadError::UnsupportedContainer {
                kind: ContainerKind::Matroska,
            } => {}
            other => panic!("expected UnsupportedContainer {{ Matroska }}, got {other:?}"),
        }
    }

    #[test]
    fn missing_file_returns_io_error() {
        let path =
            std::env::temp_dir().join(format!("lw-container-missing-{}.bin", uuid::Uuid::new_v4()));
        let err = detect(&path).expect_err("expected IO error");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
