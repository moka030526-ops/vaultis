//! Scanning a volume file frame by frame (used to rebuild a lost manifest), including
//! bounded resynchronization past a corrupt region.

use super::*;

/// Apply the manifest entry cap to a REBUILT manifest, the same way `load_manifest`
/// applies it to a stored one.
///
/// [`scan_volume`] is deliberately infallible — it returns whatever intact prefix it found
/// — so the rebuild path was the one way an over-cap manifest could enter memory. From
/// there the next `put`/`remove` in that partition would COMMIT it, and every subsequent
/// open would then hit `load_manifest`'s `TooLarge`, which [`VolumeStore::open`] does NOT
/// treat as rebuildable: an otherwise-intact vault would be permanently unopenable.
/// Rejecting at the rebuild fails closed with the same error and destroys nothing.
///
/// (`target_partition` keeps a legitimately-written partition under the cap, so this is
/// unreachable for a vault this code produced — it exists so the read side cannot admit
/// something the write side would refuse.)
pub(super) fn reject_over_cap(manifest: Manifest, max_entries: usize) -> Result<Manifest, StorageError> {
    if manifest.entries.len() > max_entries {
        return Err(StorageError::TooLarge);
    }
    Ok(manifest)
}

/// Scan a self-describing volume (any `Read + Seek`) from the start, decrypting
/// frame after frame, and rebuild its manifest up to the last good frame. Any
/// torn / garbage / foreign / undersized frame ends the scan, so this never
/// fails — it returns whatever prefix is intact. Used by both the on-disk
/// rebuild path and the fuzzer.
// Generic over `R` with the trait bound `R: Read + Seek` — i.e. `f` can be any type
// that can be read and seeked (a real `File`, or an in-memory `Cursor` in tests/fuzz).
// Returns a `Manifest` directly (never errors): it stops at the first bad frame.
pub(super) fn scan_volume<R: Read + Seek>(f: &mut R, file_len: u64, key: &Key, aad: &[u8]) -> Manifest {
    scan_volume_from(f, file_len, 0, key, aad)
}

/// [`scan_volume`] starting at an arbitrary `start` offset rather than the beginning.
///
/// Used to read only the region a recovered manifest does not already account for —
/// see [`VolumeStore::merge_volume_tail`]. Frames are self-describing and never move,
/// so a scan that begins at a frame boundary is exactly as sound as one from byte 0.
pub(super) fn scan_volume_from<R: Read + Seek>(f: &mut R, file_len: u64, start: u64, key: &Key, aad: &[u8]) -> Manifest {
    let mut offset = start;
    let mut resyncs = 0u32; // damaged regions stepped over so far (bounded, see MAX_RESYNCS)
    // Last write wins for a repeated id (updates append a newer frame).
    let mut latest: BTreeMap<String, ManifestEntry> = BTreeMap::new(); // id -> newest entry seen
    let mut order: Vec<String> = Vec::new(); // preserve first-seen order of ids
    while offset.checked_add(FRAME_PREFIX_LEN).is_some_and(|end| end <= file_len) {
        match read_frame_at(f, file_len, offset, 0, key, aad) {
            Ok((id, path, bytes)) => {
                // read_frame_at(length=0) parsed the prefix to learn the size;
                // recover the on-disk frame length to advance.
                // `let Ok(..) = .. else { break }`: on failure, stop scanning.
                let Ok(frame_len) = frame_total_len(f, offset) else { break };
                let entry = ManifestEntry {
                    id: id.clone(),
                    path,
                    size: bytes.len() as u64,
                    offset,
                    length: frame_len,
                    uploaded_at: 0,
                };
                // `insert` returns the previous value for this key (or None). If it
                // was None this is the first sighting, so record its order.
                if latest.insert(id.clone(), entry).is_none() {
                    order.push(id);
                }
                offset += frame_len; // advance to the next frame
            }
            // A frame that will not decrypt. It used to end the scan outright, which
            // meant one rotted frame in the middle of a volume cost every LATER frame
            // too — they are intact on disk, but nothing indexes them any more, and a
            // record still pointing at one of them makes the whole vault refuse to open.
            // Step over the damage instead and carry on; `resync_to_next_frame` only
            // returns an offset whose frame actually passes its AEAD tag, so nothing
            // unauthenticated can be admitted by resyncing.
            Err(_) => {
                if resyncs >= MAX_RESYNCS {
                    break; // a volume this damaged is a restore-from-backup case
                }
                let Some(next) = resync_to_next_frame(f, file_len, offset, key, aad) else { break };
                resyncs += 1;
                offset = next;
            }
        }
    }
    // Reassemble entries in first-seen order: `into_iter()` consumes `order` (moving
    // each id out), `filter_map` keeps only ids still in the map (and removes them),
    // `collect()` gathers the results into a Vec.
    let entries: Vec<ManifestEntry> = order.into_iter().filter_map(|id| latest.remove(&id)).collect();
    Manifest { seq: 1, end_offset: offset, entries }
}

/// How far past a damaged frame [`resync_to_next_frame`] will look for the next good
/// one, how many decrypt attempts it will spend doing so, and how many separate damaged
/// regions one scan will step over.
///
/// All three exist to bound the work: the search is what makes a scan of a hostile or
/// thoroughly garbled volume super-linear, and `scan_volume` is reachable from untrusted
/// input (it is a fuzz target). Sized for the failure this is actually for — localized
/// rot, a torn write, a bad sector — not for reassembling a shredded file.
pub(super) const RESYNC_WINDOW: u64 = 64 * 1024;

pub(super) const RESYNC_ATTEMPTS: u32 = 128;

pub(super) const MAX_RESYNCS: u32 = 64;

/// Cumulative frame bytes one resync will READ before giving up.
///
/// The attempt cap alone bounds the number of decrypt attempts but not their size, and
/// a frame may legitimately be up to [`MAX_DOC_SIZE`]. A volume whose bytes all decode
/// as plausible large lengths — crafted, or simply a big file damaged across a wide
/// region — therefore cost 128 × up to 64 MiB of reads per damaged region: measured at
/// 3.3 s for a 64 MiB volume, and it scales with the file (audit 2026-08-03 A-2).
///
/// Sized at twice [`MAX_DOC_SIZE`] on purpose. The budget is spent only by attempts that
/// FAIL, and the bytes a walk crosses on the way to a good frame are damaged or random —
/// so a budget near one document's size can be exhausted by an unlucky run of plausible
/// lengths in that garbage BEFORE reaching a legitimate maximum-size frame, losing it.
/// Two documents' worth leaves room for that while still bounding one damaged region at
/// roughly a second of reads instead of the ~8 GiB the unbudgeted walk allowed.
///
/// The residual is bounded and never silent corruption: if a walk does exhaust its
/// budget, the frames beyond it are simply not indexed by that rebuild, exactly as if the
/// scan had stopped — and a spare manifest, when there is one, avoids the scan entirely.
pub(super) const RESYNC_READ_BUDGET: u64 = 2 * MAX_DOC_SIZE;

/// The offset of the next frame after `from` that decrypts, or `None` if there is no
/// such frame within [`RESYNC_WINDOW`] bytes.
///
/// Two candidate sources, cheapest first:
/// 1. the damaged frame's own length prefix — intact whenever the rot landed in the
///    ciphertext, which points exactly at the next frame for the price of one attempt;
/// 2. otherwise every byte offset in the window whose length prefix is even plausible,
///    which is what recovers the case where the damage spans a frame BOUNDARY and takes
///    the next frame's length prefix with it.
///
/// A candidate is accepted only by passing its AEAD tag, so a wrong guess cannot admit
/// forged or misaligned data — it just costs an attempt. Always returns an offset
/// strictly greater than `from`, so the caller's scan cannot stall.
pub(super) fn resync_to_next_frame<R: Read + Seek>(
    f: &mut R,
    file_len: u64,
    from: u64,
    key: &Key,
    aad: &[u8],
) -> Option<u64> {
    // (1) The fast path: trust only the length, and only far enough to try one frame.
    if let Ok(total) = frame_total_len(f, from)
        && let Some(next) = from.checked_add(total)
        && next > from
        && next < file_len
        && read_frame_at(f, file_len, next, 0, key, aad).is_ok()
    {
        return Some(next);
    }

    // (2) The byte-wise walk. Read the window once rather than seeking per candidate:
    // the prefix check is then a cheap slice read, and only a plausible length costs a
    // decrypt attempt.
    let start = from.checked_add(1)?;
    let end = start.saturating_add(RESYNC_WINDOW).min(file_len);
    let span = end.checked_sub(start)?;
    if span < FRAME_PREFIX_LEN {
        return None;
    }
    let mut window = vec![0u8; span as usize];
    f.seek(SeekFrom::Start(start)).ok()?;
    f.read_exact(&mut window).ok()?;

    let mut attempts = 0u32;
    let mut spent = 0u64; // frame bytes read by attempts that failed, see RESYNC_READ_BUDGET
    // `saturating_sub` keeps the range empty (rather than wrapping) on a window too
    // short to hold a prefix — already guarded above, but this must not underflow.
    for i in 0..window.len().saturating_sub(FRAME_PREFIX_LEN as usize) {
        if attempts >= RESYNC_ATTEMPTS {
            break;
        }
        // The 4-byte length prefix at this offset, read straight out of the window.
        let len_bytes: [u8; 4] = window[i..i + 4].try_into().ok()?;
        let frame_len = u32::from_le_bytes(len_bytes) as u64;
        // The same bounds `read_frame_at` enforces, applied here first so an absurd
        // length costs a comparison instead of a decrypt.
        if frame_len < NONCE_LEN as u64 || frame_len > MAX_DOC_SIZE + 4096 {
            continue;
        }
        let at = start.saturating_add(i as u64);
        if at.saturating_add(FRAME_PREFIX_LEN).saturating_add(frame_len) > file_len {
            continue;
        }
        // Stop once the FAILED attempts have read enough. The test is on what has
        // already been spent, never on how big this candidate is: a genuine document can
        // be up to MAX_DOC_SIZE, and refusing to try it for being large would lose
        // exactly the frame the resync exists to find. The overshoot is therefore one
        // frame — bounded by MAX_DOC_SIZE — and only a RUN of large failures is cut off.
        if spent > RESYNC_READ_BUDGET {
            break;
        }
        attempts += 1;
        if read_frame_at(f, file_len, at, 0, key, aad).is_ok() {
            return Some(at);
        }
        spent = spent.saturating_add(frame_len);
    }
    None
}
