//! Walk top-level ISO BMFF atoms in an mp4/mov file and extract just the
//! container metadata the server needs to run a quality check.
//!
//! Atom layout (top level):
//!   * 4 bytes: `size` (big-endian `u32`). If `0`, atom extends to EOF. If `1`,
//!     the real size is a `u64` immediately after the type field (large-size
//!     form).
//!   * 4 bytes: `type` (4 ASCII chars).
//!   * payload follows: `size - 8` bytes (or `size - 16` for large-size).
//!
//! For each top-level atom *before* `moov`, ship the full payload when the
//! atom is small (≤ 64 KiB — `ftyp`, `free`, `wide`) and only the 8- or
//! 16-byte header otherwise (`mdat`, `mfra`). The server reconstructs a
//! sparse temp file from `(absolute_offset, bytes)` chunks and runs ffprobe
//! against it; ffprobe walks atoms top-down by reading each header and
//! seeking past the payload, so headers-only-of-mdat is enough for it to
//! reach `moov`.
//!
//! The `moov` payload itself is shipped verbatim at its real offset.
//!
//! Total wire cost stays sub-1 MiB even on multi-GB camera files; we cap at
//! [`MAX_PAYLOAD_BYTES`] (8 MiB) and bail with [`VideoValidationError::MoovTooLarge`]
//! for anything beyond.

use crate::error::VideoValidationError;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Hard cap on the assembled payload. Real-world finalized clips top out
/// near 1 MiB; anything past 8 MiB is malformed or hostile.
pub const MAX_PAYLOAD_BYTES: u64 = 8 * 1024 * 1024;

/// Atoms below this size go on the wire in full; atoms above only ship
/// their 8- or 16-byte header. Picked so `ftyp`, `free`, `wide`, and
/// other small prelude boxes always travel intact while `mdat`'s media
/// payload never does.
const FULL_COPY_THRESHOLD: u64 = 64 * 1024;

/// Size of a plain atom header.
const HEADER_LEN: u64 = 8;

/// Size of a large-size-form atom header (`size32 == 1`, real `size`
/// follows as a `u64`).
const LARGE_HEADER_LEN: u64 = 16;

/// Sparse layout the server reconstructs into a temp file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtomChunks {
    /// Each `(absolute_offset, bytes)` pair, in file order. The server
    /// `seek + write`s each pair into a sparse temp file of size
    /// [`Self::total_size`].
    pub chunks: Vec<(u64, Vec<u8>)>,
    /// Total length of the original file. Used by the server to
    /// `setLength` the sparse temp file before writing chunks.
    pub total_size: u64,
}

impl AtomChunks {
    /// Sum of the bytes shipped on the wire.
    pub fn payload_bytes(&self) -> u64 {
        self.chunks.iter().map(|(_, b)| b.len() as u64).sum()
    }
}

/// One top-level atom's header descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Atom {
    /// 4-character ASCII type code.
    fourcc: [u8; 4],
    /// Absolute offset of the atom header in the source file.
    offset: u64,
    /// Total atom size in bytes, including the header.
    size: u64,
    /// Header length: [`HEADER_LEN`] for normal atoms, [`LARGE_HEADER_LEN`]
    /// for large-size-form atoms.
    header_len: u64,
}

impl Atom {
    fn type_str(&self) -> &str {
        // `walk_atoms` rejects non-ASCII fourcc bytes, so this is always
        // valid utf-8. We use the slice form rather than `from_utf8_unchecked`
        // because the unchecked variant breaks on a corrupt input that
        // somehow snuck past our filter.
        std::str::from_utf8(&self.fourcc).unwrap_or("")
    }
}

/// Walk every top-level atom, then assemble the sparse layout the server
/// needs to reconstruct a probe-able file.
///
/// Errors:
///   * [`VideoValidationError::Unplayable`] — no `moov` atom is present
///     (typical for power-cut recordings) or the file is too short to be
///     ISO BMFF.
///   * [`VideoValidationError::MoovTooLarge`] — assembled payload would
///     exceed [`MAX_PAYLOAD_BYTES`].
///   * [`VideoValidationError::Io`] — read from the input file failed.
pub fn extract_atom_chunks(path: &Path) -> Result<AtomChunks, VideoValidationError> {
    let mut file = File::open(path)?;
    let total_size = file.metadata()?.len();

    let atoms = walk_atoms(&mut file, total_size)?;
    let Some(moov_index) = atoms.iter().position(|a| &a.fourcc == b"moov") else {
        return Err(VideoValidationError::Unplayable {
            reason: "no moov atom (file is unfinalized or truncated)".to_string(),
        });
    };

    // Top-level atoms before `moov` ship full payload when small, header-only
    // when large; `moov` itself ships verbatim. Everything after `moov` is
    // skipped — `mfra` and friends are not load-bearing for ffprobe.
    let mut chunks: Vec<(u64, Vec<u8>)> = Vec::with_capacity(moov_index + 1);
    let mut total_payload: u64 = 0;
    for atom in &atoms[..moov_index] {
        let bytes = read_atom_chunk(&mut file, atom)?;
        total_payload = total_payload.saturating_add(bytes.len() as u64);
        if total_payload > MAX_PAYLOAD_BYTES {
            return Err(VideoValidationError::MoovTooLarge {
                bytes: total_payload,
                cap: MAX_PAYLOAD_BYTES,
            });
        }
        chunks.push((atom.offset, bytes));
    }

    let moov = atoms[moov_index];
    let moov_bytes = read_full_atom(&mut file, &moov)?;
    total_payload = total_payload.saturating_add(moov_bytes.len() as u64);
    if total_payload > MAX_PAYLOAD_BYTES {
        return Err(VideoValidationError::MoovTooLarge {
            bytes: total_payload,
            cap: MAX_PAYLOAD_BYTES,
        });
    }
    chunks.push((moov.offset, moov_bytes));

    Ok(AtomChunks { chunks, total_size })
}

/// Read either the full atom (when small) or its header (when large). The
/// 64 KiB threshold is the line between "useful prelude metadata" and
/// "raw media payload we don't want to ship".
fn read_atom_chunk(file: &mut File, atom: &Atom) -> Result<Vec<u8>, VideoValidationError> {
    if atom.size <= FULL_COPY_THRESHOLD {
        read_full_atom(file, atom)
    } else {
        read_header_only(file, atom)
    }
}

fn read_full_atom(file: &mut File, atom: &Atom) -> Result<Vec<u8>, VideoValidationError> {
    file.seek(SeekFrom::Start(atom.offset))?;
    let mut buf = vec![0u8; atom.size as usize];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

fn read_header_only(file: &mut File, atom: &Atom) -> Result<Vec<u8>, VideoValidationError> {
    file.seek(SeekFrom::Start(atom.offset))?;
    let mut buf = vec![0u8; atom.header_len as usize];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

/// Walk all top-level atoms in `file`. Returns an empty vec if the file
/// is too short or the very first header is unparseable — callers detect
/// "no moov" downstream and surface the right `Unplayable` reason.
fn walk_atoms(file: &mut File, total_size: u64) -> Result<Vec<Atom>, VideoValidationError> {
    let mut atoms: Vec<Atom> = Vec::new();
    let mut offset: u64 = 0;
    while offset < total_size {
        let Some(atom) = read_atom_header(file, offset, total_size)? else {
            break;
        };
        let next =
            offset
                .checked_add(atom.size)
                .ok_or_else(|| VideoValidationError::Unplayable {
                    reason: format!(
                        "atom '{}' at offset {offset} overflows file size",
                        atom.type_str()
                    ),
                })?;
        atoms.push(atom);
        offset = next;
    }
    Ok(atoms)
}

/// Read one atom header at `offset`. Returns `Ok(None)` on a clean
/// short read at EOF (we've reached the end of a well-formed file);
/// returns an `Err(Unplayable)` on a corrupt or truncated header.
fn read_atom_header(
    file: &mut File,
    offset: u64,
    total_size: u64,
) -> Result<Option<Atom>, VideoValidationError> {
    if total_size.saturating_sub(offset) < HEADER_LEN {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(offset))?;

    let mut header = [0u8; 8];
    file.read_exact(&mut header)?;
    let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    let fourcc = [header[4], header[5], header[6], header[7]];
    if !fourcc.iter().all(|b| b.is_ascii() && !b.is_ascii_control()) {
        // Not a real atom — bail. The file is non-ISO-BMFF or corrupt
        // past this point.
        return Ok(None);
    }

    let (size, header_len) = match size32 {
        1 => {
            // Large-size form: real size is a u64 right after the type.
            if total_size.saturating_sub(offset) < LARGE_HEADER_LEN {
                return Err(VideoValidationError::Unplayable {
                    reason: format!("truncated large-size atom header at offset {offset}"),
                });
            }
            let mut large = [0u8; 8];
            file.read_exact(&mut large)?;
            (u64::from_be_bytes(large), LARGE_HEADER_LEN)
        }
        0 => {
            // size==0 means the atom extends to EOF. Only valid for the
            // last atom. We treat it as a normal atom of computed length.
            (total_size - offset, HEADER_LEN)
        }
        n => (u64::from(n), HEADER_LEN),
    };

    if size < header_len || offset.checked_add(size).is_none_or(|end| end > total_size) {
        return Err(VideoValidationError::Unplayable {
            reason: format!(
                "atom at offset {offset} has bogus size {size} (file ends at {total_size})"
            ),
        });
    }

    Ok(Some(Atom {
        fourcc,
        offset,
        size,
        header_len,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write `size32` + 4-byte type as a normal atom header.
    fn header(size: u32, fourcc: &[u8; 4]) -> Vec<u8> {
        let mut out = Vec::with_capacity(8);
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(fourcc);
        out
    }

    /// Build a minimal `ftyp` payload. Total length: 24 bytes including
    /// the header. Mirrors the standard major+minor+compat-list shape so
    /// the header walker has plausible content to step over.
    fn ftyp_atom() -> Vec<u8> {
        let mut atom = header(24, b"ftyp");
        atom.extend_from_slice(b"isom"); // major brand
        atom.extend_from_slice(&0u32.to_be_bytes()); // minor version
        atom.extend_from_slice(b"isomavc1"); // compatible brands
        assert_eq!(atom.len(), 24);
        atom
    }

    /// Atom with the given fourcc and a payload of `payload_len` zero
    /// bytes. `payload_len + 8` must fit in u32; for large bodies use
    /// [`large_atom`] instead.
    fn small_atom(fourcc: &[u8; 4], payload_len: usize) -> Vec<u8> {
        let total = (payload_len + 8) as u32;
        let mut atom = header(total, fourcc);
        atom.extend(std::iter::repeat_n(0u8, payload_len));
        atom
    }

    /// Build a large-size-form atom: 4-byte size of `1`, 4-byte type,
    /// 8-byte u64 real size, then `payload_len` zero bytes.
    fn large_atom(fourcc: &[u8; 4], payload_len: u64) -> Vec<u8> {
        let real_size = LARGE_HEADER_LEN + payload_len;
        let mut atom = Vec::with_capacity(real_size as usize);
        atom.extend_from_slice(&1u32.to_be_bytes());
        atom.extend_from_slice(fourcc);
        atom.extend_from_slice(&real_size.to_be_bytes());
        atom.extend(std::iter::repeat_n(0u8, payload_len as usize));
        atom
    }

    fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("lw-test-{name}-{}.mp4", uuid::Uuid::new_v4()));
        let mut f = std::fs::File::create(&path).expect("create temp");
        f.write_all(bytes).expect("write temp");
        path
    }

    /// Faststart layout: ftyp || moov || mdat. The whole moov payload ships,
    /// the mdat header ships header-only (mdat is much larger than 64 KiB).
    #[test]
    fn faststart_layout_ships_full_moov_and_mdat_header_only() {
        let ftyp = ftyp_atom();
        // moov small enough to ship in full (well below 64 KiB).
        let moov_payload_len = 256;
        let moov = small_atom(b"moov", moov_payload_len);
        // mdat is large — must travel header-only.
        let mdat_payload_len = 80 * 1024; // 80 KiB > 64 KiB threshold
        let mdat = small_atom(b"mdat", mdat_payload_len);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&ftyp);
        bytes.extend_from_slice(&moov);
        bytes.extend_from_slice(&mdat);
        let total = bytes.len() as u64;

        let path = write_temp("faststart", &bytes);
        let extracted = extract_atom_chunks(&path).expect("extract");
        let _ = std::fs::remove_file(&path);

        assert_eq!(extracted.total_size, total);
        // chunks: ftyp (full) and moov (full). Anything after moov isn't shipped.
        assert_eq!(extracted.chunks.len(), 2);

        let (ftyp_off, ftyp_bytes) = &extracted.chunks[0];
        assert_eq!(*ftyp_off, 0);
        assert_eq!(ftyp_bytes.len(), ftyp.len());

        let (moov_off, moov_bytes) = &extracted.chunks[1];
        assert_eq!(*moov_off, ftyp.len() as u64);
        assert_eq!(moov_bytes.len(), moov.len());
    }

    /// moov-at-tail layout: ftyp || mdat (huge) || moov. The mdat header
    /// must ship so ffprobe can step past it; the moov payload must ship
    /// in full at its real offset.
    #[test]
    fn moov_at_tail_layout_ships_mdat_header_then_full_moov() {
        let ftyp = ftyp_atom();
        let mdat_payload_len = 200 * 1024; // > 64 KiB threshold
        let mdat = small_atom(b"mdat", mdat_payload_len);
        let moov_payload_len = 512;
        let moov = small_atom(b"moov", moov_payload_len);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&ftyp);
        bytes.extend_from_slice(&mdat);
        bytes.extend_from_slice(&moov);
        let total = bytes.len() as u64;

        let path = write_temp("moov-tail", &bytes);
        let extracted = extract_atom_chunks(&path).expect("extract");
        let _ = std::fs::remove_file(&path);

        assert_eq!(extracted.total_size, total);
        // ftyp full + mdat header-only + moov full = 3 chunks.
        assert_eq!(extracted.chunks.len(), 3);

        let (mdat_off, mdat_bytes) = &extracted.chunks[1];
        assert_eq!(*mdat_off, ftyp.len() as u64);
        assert_eq!(mdat_bytes.len(), HEADER_LEN as usize);

        let (moov_off, moov_bytes) = &extracted.chunks[2];
        let expected_moov_off = (ftyp.len() + mdat.len()) as u64;
        assert_eq!(*moov_off, expected_moov_off);
        assert_eq!(moov_bytes.len(), moov.len());
    }

    /// Large-size-form mdat (size32==1, real size in following u64). The
    /// header chunk must be 16 bytes, not 8.
    #[test]
    fn large_size_mdat_ships_16_byte_header() {
        let ftyp = ftyp_atom();
        let mdat_payload_len: u64 = 200 * 1024;
        let mdat = large_atom(b"mdat", mdat_payload_len);
        let moov = small_atom(b"moov", 256);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&ftyp);
        bytes.extend_from_slice(&mdat);
        bytes.extend_from_slice(&moov);

        let path = write_temp("large-mdat", &bytes);
        let extracted = extract_atom_chunks(&path).expect("extract");
        let _ = std::fs::remove_file(&path);

        assert_eq!(extracted.chunks.len(), 3);
        let (_, mdat_bytes) = &extracted.chunks[1];
        assert_eq!(mdat_bytes.len(), LARGE_HEADER_LEN as usize);
        // First 4 bytes must be the size32==1 marker.
        assert_eq!(mdat_bytes[..4], 1u32.to_be_bytes());
    }

    /// No moov atom present — typical for an interrupted/power-cut
    /// recording. The walker bails with `Unplayable` so the desktop
    /// fails-fast without sending bytes to the server.
    #[test]
    fn missing_moov_returns_unplayable() {
        let ftyp = ftyp_atom();
        let mdat = small_atom(b"mdat", 64);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&ftyp);
        bytes.extend_from_slice(&mdat);

        let path = write_temp("no-moov", &bytes);
        let result = extract_atom_chunks(&path);
        let _ = std::fs::remove_file(&path);

        match result {
            Err(VideoValidationError::Unplayable { reason }) => {
                assert!(
                    reason.contains("moov"),
                    "reason should mention moov: {reason}"
                );
            }
            other => panic!("expected Unplayable, got {other:?}"),
        }
    }

    /// moov payload exceeding the 8 MiB cap must reject before the
    /// payload is allocated. Use a large-size moov so we can express
    /// >64 KiB without exhausting u32.
    #[test]
    fn oversized_moov_returns_too_large() {
        let ftyp = ftyp_atom();
        // 9 MiB moov payload — well past the cap.
        let huge_payload: u64 = 9 * 1024 * 1024;
        let moov = large_atom(b"moov", huge_payload);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&ftyp);
        bytes.extend_from_slice(&moov);

        let path = write_temp("huge-moov", &bytes);
        let result = extract_atom_chunks(&path);
        let _ = std::fs::remove_file(&path);

        match result {
            Err(VideoValidationError::MoovTooLarge { bytes, cap }) => {
                assert!(bytes > cap, "bytes {bytes} should exceed cap {cap}");
                assert_eq!(cap, MAX_PAYLOAD_BYTES);
            }
            other => panic!("expected MoovTooLarge, got {other:?}"),
        }
    }

    /// A sub-64 KiB free atom before moov should ship in full.
    #[test]
    fn small_pre_moov_atoms_ship_in_full() {
        let ftyp = ftyp_atom();
        let free = small_atom(b"free", 16);
        let moov = small_atom(b"moov", 128);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&ftyp);
        bytes.extend_from_slice(&free);
        bytes.extend_from_slice(&moov);

        let path = write_temp("free-prelude", &bytes);
        let extracted = extract_atom_chunks(&path).expect("extract");
        let _ = std::fs::remove_file(&path);

        assert_eq!(extracted.chunks.len(), 3);
        let (free_off, free_bytes) = &extracted.chunks[1];
        assert_eq!(*free_off, ftyp.len() as u64);
        assert_eq!(free_bytes.len(), free.len());
    }
}
