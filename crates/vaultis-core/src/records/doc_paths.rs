//! Where attached documents live inside the vault: the per-record storage location,
//! timestamped and sanitized filenames, and display-safe rendering of untrusted names.

use super::*;

/// The virtual folder a tax year's documents live in: `taxes/<sanitized-year>`.
/// Non-alphanumeric characters in the year are dropped so the folder name is
/// always safe; an empty/blank year falls back to `taxes/unspecified`. Shared by
/// the front-ends so all store a given year's documents in the same place.
pub fn tax_doc_location(year: &str) -> String {
    let y: String = year.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if y.is_empty() { "taxes/unspecified".to_string() } else { format!("taxes/{y}") }
}

/// The virtual folder a property's documents live in: `real-estate/<sanitized>`,
/// derived from the address (alphanumeric only, lowercased, truncated), with a
/// `real-estate/property` fallback for a blank address. Shared by both UIs.
pub fn real_estate_doc_location(address: &str) -> String {
    let a: String =
        address.chars().filter(|c| c.is_ascii_alphanumeric()).take(40).collect::<String>().to_lowercase();
    if a.is_empty() { "real-estate/property".to_string() } else { format!("real-estate/{a}") }
}

/// Slugify one virtual-path component: lowercase, keep ASCII alphanumerics, turn
/// every other run into a single '-', trim leading/trailing '-', and cap the
/// length at 40. An empty result falls back to `fallback`. Used for the auto-group
/// level (document/description/title) and the optional user subfolder so the
/// volume path is always filesystem-safe and free of separators or traversal.
pub fn doc_slug(s: &str, fallback: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.truncate(40);
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() { fallback.to_string() } else { out }
}

/// Uppercased initials of an owner for the owner-first directory level: the first
/// ASCII-alphanumeric character of each whitespace-separated word, uppercased and
/// concatenated, capped at 8 chars. No connector special-casing ("Michael and Sarah"
/// -> "MAS", "Michael & Sarah" -> "MS", "Joint" -> "J"). Returns "" when the owner is
/// blank or has no alphanumeric — callers then OMIT the initials level entirely.
pub fn owner_initials(owner: &str) -> String {
    let mut out = String::new();
    for word in owner.split_whitespace() {
        if let Some(c) = word.chars().find(char::is_ascii_alphanumeric) {
            out.push(c.to_ascii_uppercase());
            if out.len() >= 8 {
                break;
            }
        }
    }
    out
}

/// Prepend the owner-initials top-level folder to a `<root>[/<group>]` base, giving the
/// owner-first layout `[<INITIALS>/]<base>`. When `owner` is `None` (tabs without an
/// owner) or the initials are empty (blank owner), `base` is returned unchanged.
pub fn owner_prefix(owner: Option<&str>, base: &str) -> String {
    match owner.map(owner_initials).filter(|i| !i.is_empty()) {
        Some(init) => format!("{init}/{base}"),
        None => base.to_string(),
    }
}

/// The per-tab `<root>[/<group>]` base for the Trust & Will and General Documents tabs
/// (the multi-doc Taxes/Real-Estate tabs have their own helpers above; Assets uses
/// [`asset_doc_location`] below = the kind root). The group is slugged from the record's
/// identifying field. The owner-initials top level (for owner-bearing tabs) is layered on
/// by [`owner_prefix`].
pub fn trust_will_doc_location(document: &str) -> String {
    format!("trust-will/{}", doc_slug(document, "document"))
}

/// The kind root for an Asset/Liability document: `liabilities` when the record's `kind`
/// is "Liability" (case-insensitive), else `assets`. Used as the `base` for
/// [`owner_prefix`], giving the owner-first `<INITIALS>/assets|liabilities`. No slugged
/// auto-group level (assets are grouped by owner, not description).
pub fn asset_doc_location(kind: &str) -> String {
    if kind.trim().eq_ignore_ascii_case("Liability") { "liabilities".to_string() } else { "assets".to_string() }
}

pub fn general_doc_location(title: &str) -> String {
    format!("general-documents/{}", doc_slug(title, "untitled"))
}

/// A compact UTC timestamp `YYYYMMDD-HHMMSS` from Unix seconds, prefixed onto each
/// uploaded filename ([`timestamped_filename`]). Sortable, fixed-width, filesystem-safe.
pub fn compact_utc(unix_secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_unix(unix_secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// True if `s` is exactly a compact-UTC stamp `YYYYMMDD-HHMMSS` (8 digits, '-', 6 digits
/// = 15 chars). Used by the throwaway migration to locate/recognize the upload timestamp
/// in a stored path or filename prefix.
pub fn is_compact_utc(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 15 || b[8] != b'-' || !b[..8].iter().all(u8::is_ascii_digit) || !b[9..].iter().all(u8::is_ascii_digit)
    {
        return false;
    }
    // Validate the date/time components are PLAUSIBLE (not merely digit-shaped) so an arbitrary
    // all-digit directory or year (e.g. "12345678-901234") can't be misread as a timestamp by the
    // migration. All slices are ASCII digits, so parse always succeeds.
    let n = |lo: usize, hi: usize| s[lo..hi].parse::<u32>().unwrap_or(u32::MAX);
    let (mo, d, h, mi, sec) = (n(4, 6), n(6, 8), n(9, 11), n(11, 13), n(13, 15));
    (1..=12).contains(&mo) && (1..=31).contains(&d) && h < 24 && mi < 60 && sec < 60
}

/// Prefix an already-sanitized filename with the upload timestamp: `<ts>_<name>`.
/// `name` must be [`doc_filename`] output (<=120 B sanitized); `ts` is [`compact_utc`]
/// (15 B). The result is <=136 B for the last path component — well under MAX_PATH_LEN.
pub fn timestamped_filename(ts: &str, name: &str) -> String {
    format!("{ts}_{name}")
}

/// Build the virtual *directory* a freshly-uploaded document is filed under:
///   `<prefix>[/<subfolder>]`
/// `prefix` is the owner-first `[<INITIALS>/]<root>[/<group>]` (see [`owner_prefix`] plus
/// the per-tab `*_doc_location` helpers); `subfolder` is the optional user level (slugged,
/// omitted when blank). The per-upload timestamp is NOT a directory level any more — it is
/// folded into the filename via [`timestamped_filename`]. The caller appends the filename
/// with `vault::virtual_path`.
pub fn doc_upload_dir(prefix: &str, subfolder: &str) -> String {
    let mut dir = prefix.to_string();
    let sub = subfolder.trim();
    if !sub.is_empty() {
        dir.push('/');
        dir.push_str(&doc_slug(sub, "subfolder"));
    }
    dir
}

/// Sanitize a user-supplied filename for the volume path: replace any whitespace
/// with `-` (so no path component contains a space), neutralize path separators and
/// control characters with `_` (so the user controls the name without injecting
/// extra path levels or `..` traversal), strip surrounding dots, and cap the length.
/// Falls back to `"file"` when nothing usable remains. Dots inside the name are kept
/// so extensions like `return.pdf` survive.
/// A Unicode formatting/bidi/zero-width char that `char::is_control` (Cc-only) does
/// NOT catch but which can still spoof how a name/path DISPLAYS — most dangerously
/// the right-to-left override U+202E, which renders `report\u{202e}txt.exe` as
/// `report exe.txt`. Rejected/neutralized in document names and untrusted paths.
///
/// The set covers two families, and deliberately stops there (audit 2026-07-25 round 2):
///
/// * **Bidi controls** — every character that can reorder the run around it. The obvious
///   overrides/embeddings/isolates, *plus* U+061C ARABIC LETTER MARK: it is the ALM
///   counterpart of LRM/RLM (U+200E/U+200F) and reorders adjacent neutrals (digits,
///   punctuation) with no override needed, so omitting it left a hole in the exact family
///   this function exists to close.
/// * **Characters that render as nothing** — zero-width, invisible operators, blank
///   fillers, and the U+E0000 TAGS block (an entire shadow ASCII alphabet that draws no
///   glyph, the modern "invisible text" smuggling vector). Two labels or filenames that
///   differ only by these are indistinguishable on screen.
///
/// It is NOT "every Unicode `Cf`". Format characters that have a legitimate *visible*
/// rendering in a living script — the Arabic/Syriac/Kaithi number and honorific signs
/// (U+0600–U+0605, U+06DD, U+070F, U+0890, U+08E2, U+110BD, U+110CD), the Egyptian
/// hieroglyph joiners, the musical beam/tie/slur marks and the shorthand overlaps — are
/// left alone, as are the variation selectors (U+FE00–U+FE0F), which a legitimate emoji
/// needs to render in colour. Neutralizing those would mangle honest labels to no
/// security end: they are not invisible and they do not reorder.
pub(crate) fn is_spoofy_format_char(c: char) -> bool {
    matches!(c,
        '\u{00AD}'                  // SOFT HYPHEN — invisible in virtually every renderer
        | '\u{061C}'                // ARABIC LETTER MARK — bidi control (the ALM twin of LRM/RLM)
        | '\u{115F}' | '\u{1160}'   // HANGUL CHOSEONG/JUNGSEONG FILLER — draw nothing
        | '\u{180E}'               // MONGOLIAN VOWEL SEPARATOR — zero-width (Cf since Unicode 6.3)
        | '\u{200B}'..='\u{200F}'   // zero-width space/joiners + LRM/RLM
        | '\u{2028}'              // LINE SEPARATOR — a real line break that char::is_control misses
        | '\u{2029}'              // PARAGRAPH SEPARATOR — likewise (keeps CSV cells one physical line)
        | '\u{202A}'..='\u{202E}' // bidi embeddings + LRO/RLO override
        | '\u{2060}'..='\u{2064}'   // word joiner + FUNCTION APPLICATION / INVISIBLE TIMES/SEPARATOR/PLUS
        | '\u{2066}'..='\u{2069}' // bidi isolates
        | '\u{206A}'..='\u{206F}'   // deprecated bidi/shaping controls (symmetric swapping, digit shapes)
        | '\u{3164}'                // HANGUL FILLER — draws nothing
        | '\u{FEFF}'              // zero-width no-break space / BOM
        | '\u{FFA0}'                // HALFWIDTH HANGUL FILLER — draws nothing
        | '\u{FFF9}'..='\u{FFFB}'   // interlinear annotation — hides the annotated run
        | '\u{E0001}'               // LANGUAGE TAG (deprecated)
        | '\u{E0020}'..='\u{E007F}' // TAGS block — an invisible shadow ASCII alphabet
    )
}

/// Replace control characters and Unicode bidi/format/zero-width characters (everything
/// [`is_spoofy_format_char`] flags, plus [`char::is_control`]) with `_`, for rendering an
/// UNTRUSTED string into a context where those characters would spoof — a terminal line, a
/// merge preview the user authorizes, or a real on-disk filename. Unlike [`doc_filename`] it
/// does NOT touch separators/whitespace or cap length; it only neutralizes the invisible/bidi
/// spoof set, so it is safe to apply to an arbitrary display label without otherwise mangling it.
/// `pub` (not `pub(crate)`) so the desktop CLI `extract` (main.rs) can apply the same
/// neutralization to a filename derived from an untrusted manifest path.
pub fn display_safe(s: &str) -> String {
    s.chars().map(|c| if c.is_control() || is_spoofy_format_char(c) { '_' } else { c }).collect()
}

/// True if `name`'s stem (the part before the first '.') is a Windows reserved DEVICE name
/// (case-insensitive): CON, PRN, AUX, NUL, CONIN$, CONOUT$, COM1–9, LPT1–9. On Windows such
/// a name maps to a device, not a file, regardless of extension (`con.pdf` opens the
/// console), so it must be neutralized before becoming a real filesystem path component on
/// export — otherwise an heir extracting on Windows gets an I/O error instead of the document.
///
/// `pub` so the desktop `extract` CLI shares this one definition rather than keeping its own
/// copy (which had drifted: it recognized a shorter list and *dropped* the offending
/// component instead of renaming it, so the same vault extracted to a different tree
/// depending on which front-end wrote it — audit 2026-07-25 round 2).
///
/// Two forms beyond the classic list are covered because Windows folds them onto a device:
/// the console handles `CONIN$`/`CONOUT$`, and the SUPERSCRIPT digit spellings `COM¹`/`COM²`/
/// `COM³` (U+00B9/U+00B2/U+00B3), which its path canonicalization maps to `COM1`/`COM2`/`COM3`.
pub fn is_windows_reserved_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    // Uppercase, and fold the superscript digits onto their ASCII twins, so `com¹` is
    // recognized as `COM1`. Both source chars are 2 bytes and both replacements are 1, so
    // the byte-length test below still sees a 4-byte `COM1`-shaped stem.
    let s: String = stem
        .chars()
        .map(|c| match c {
            '\u{00B9}' => '1',
            '\u{00B2}' => '2',
            '\u{00B3}' => '3',
            _ => c.to_ascii_uppercase(),
        })
        .collect();
    matches!(s.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        // `len() == 4` is in BYTES, but `starts_with("COM")` pins the first three to ASCII,
        // so a 4-byte match always has a single-byte 4th character to index.
        || (s.len() == 4
            && (s.starts_with("COM") || s.starts_with("LPT"))
            && matches!(s.as_bytes()[3], b'1'..=b'9'))
}

pub fn doc_filename(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_whitespace() {
                '-' // no spaces (or tabs/newlines) anywhere in a volume path
            } else if c == '/' || c == '\\' || c.is_control() || is_spoofy_format_char(c) {
                '_' // neutralize separators, control, AND bidi/zero-width spoof chars
            } else {
                c
            }
        })
        .collect();
    // Cap at 120 bytes, truncating on a UTF-8 char boundary. A raw `truncate(120)`
    // PANICS when byte 120 lands mid-character (multibyte name: accented Latin, CJK,
    // emoji, …), so step the cut back to the nearest boundary first. Inner helper so the
    // cap can be re-applied AFTER the reserved-name prefix below (which can add a byte).
    fn cap_120(s: &mut String) {
        if s.len() > 120 {
            let mut cut = 120;
            while cut > 0 && !s.is_char_boundary(cut) {
                cut -= 1;
            }
            s.truncate(cut);
        }
    }
    cap_120(&mut out);
    // Strip leading/trailing dots and dashes (whitespace is already mapped to `-`),
    // so a dot/space-only name collapses to the fallback rather than "--..".
    let trimmed = out.trim_matches(|c: char| c == '.' || c == '-');
    if trimmed.is_empty() {
        return "file".to_string();
    }
    // Neutralize a Windows reserved device name so the stored (and later exported) file is a
    // real, distinct file on Windows rather than the CON/NUL/COM1/… device. Harmless on Unix.
    let mut out = if is_windows_reserved_name(trimmed) { format!("_{trimmed}") } else { trimmed.to_string() };
    // The reserved-name '_' prefix can push a name that was already at the 120-byte cap to
    // 121, so re-cap (on a char boundary) and re-trim any trailing dot/dash the cut exposed,
    // keeping BOTH the length and no-edge-dot invariants. (Caught by the doc_paths fuzz target.)
    if out.len() > 120 {
        cap_120(&mut out);
        let keep = out.trim_end_matches(['.', '-']).len();
        out.truncate(keep);
    }
    if out.is_empty() { "file".to_string() } else { out }
}

/// Resolve the upload filename: the user-typed `name` if non-empty (trimmed), else the
/// **basename of the `source` path** ("if a filename isn't specified, use the same
/// filename as the file being uploaded"). The result is NOT yet sanitized — callers run
/// it through [`doc_filename`]. Returns `""` only if both are empty / the source has no
/// final component, which callers reject.
pub fn effective_doc_filename(name: &str, source: &str) -> String {
    let n = name.trim();
    if !n.is_empty() {
        return n.to_string();
    }
    std::path::Path::new(unquote_path(source))
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Normalize a user-typed "upload from" source path: trim surrounding whitespace, then
/// strip a single MATCHED pair of surrounding ASCII double quotes. File managers'
/// "Copy as path" (Windows Explorer) and shells wrap a path — especially one containing
/// spaces — in double quotes, so accept that form and let the user paste it directly.
/// Only a matched leading+trailing pair is removed; the content INSIDE the quotes is left
/// exactly as-is (quotes preserve inner spaces, matching shell semantics), and a lone
/// quote at just one end is left alone (it is a legitimate, if unusual, path character).
pub fn unquote_path(s: &str) -> &str {
    let t = s.trim();
    // `strip_prefix`/`strip_suffix` return `None` when the affix is absent. Require BOTH
    // (and length >= 2 so a single `"` isn't treated as an empty quoted string).
    if t.len() >= 2
        && let Some(inner) = t.strip_prefix('"').and_then(|r| r.strip_suffix('"'))
    {
        inner
    } else {
        t
    }
}
