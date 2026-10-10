//! Record search: case-insensitive substring matching plus the "sounds like" (Soundex)
//! fallback.

/// Case-insensitive substring match used by the UIs' free-text search (e.g.
/// searching accounts by username). An empty/whitespace-only `query` matches
/// everything (no filter). Both sides are lower-cased and the query is trimmed.
// `haystack`/`query` are borrowed `&str`; the function only reads them.
pub fn matches_search(haystack: &str, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    q.is_empty() || haystack.to_lowercase().contains(&q)
}

/// Consonant class of an ASCII letter (lower-cased): `Some(digit)` for a coded consonant,
/// `None` for a vowel-like letter (a/e/i/o/u/y — and h/w, which [`soundex`] treats specially).
/// Shared by [`soundex`] and [`soundex_key`] so both agree on what "sounds the same".
pub(super) fn soundex_class(c: char) -> Option<u8> {
    match c {
        'b' | 'f' | 'p' | 'v' => Some(b'1'),
        'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => Some(b'2'),
        'd' | 't' => Some(b'3'),
        'l' => Some(b'4'),
        'm' | 'n' => Some(b'5'),
        'r' => Some(b'6'),
        _ => None,
    }
}

/// The American **Soundex** code of one word (`"Robert" -> "R163"`), or `None` when the word
/// holds no ASCII letter to seed a code (digits, punctuation, or non-ASCII script).
///
/// Soundex maps a word to its first letter plus three consonant-class digits, so spellings that
/// SOUND alike collapse to the same code (`Smith`/`Smyth`, `Nguyen`/`Nguyan`). It is deliberately
/// ASCII-only and deliberately crude: it is used to WIDEN a search, never to narrow one, so a
/// word it cannot code simply falls back to the substring rule in [`matches_search_soundlike`].
///
/// Non-letters are skipped; h/w are transparent (they don't break a repeat) and vowels reset the
/// "previous class", so `Tymczak`-style repeats across a vowel still yield two digits. The result
/// is always exactly 4 chars (zero-padded). This is the TEXTBOOK code, which keeps the first
/// letter verbatim; the search matcher compares [`soundex_key`]s so `Katherine`/`Catherine` meet.
pub fn soundex(word: &str) -> Option<String> {
    let mut letters = word.chars().filter(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_lowercase());
    let first = letters.next()?;
    let mut code = String::with_capacity(4);
    code.push(first.to_ascii_uppercase());
    // `prev` is the class of the last letter that was NOT h/w — that is what a repeat is
    // measured against, which is why h/w are skipped without touching it.
    let mut prev = soundex_class(first);
    for c in letters {
        if c == 'h' || c == 'w' {
            continue; // transparent: "Ashcraft" codes A261, not A226
        }
        let cur = soundex_class(c);
        if let Some(d) = cur
            && cur != prev
        {
            code.push(d as char);
        }
        prev = cur; // a vowel sets `prev` to None, so a repeat after it IS coded
        if code.len() == 4 {
            break;
        }
    }
    while code.len() < 4 {
        code.push('0');
    }
    Some(code)
}

/// The comparison key used by [`matches_search_soundlike`]: the word's [`soundex`] code with its
/// INITIAL letter also folded to its consonant class (`Katherine -> "2365"`, `Catherine -> "2365"`).
///
/// Textbook Soundex keeps the first letter as-is, which is exactly where sound-alike spellings
/// differ most often for names people actually search — Katherine/Catherine, Chris/Kris,
/// Fisher/Visher. Folding it makes those meet. A vowel/h/w/y initial has no class, so it is kept
/// verbatim (Allen and Ellen genuinely start differently).
pub fn soundex_key(word: &str) -> Option<String> {
    let code = soundex(word)?;
    let mut chars = code.chars();
    let first = chars.next()?; // `soundex` always yields 4 chars, so this is infallible
    let folded = match soundex_class(first.to_ascii_lowercase()) {
        Some(d) => d as char,
        None => first,
    };
    Some(std::iter::once(folded).chain(chars).collect())
}

/// The fewest ASCII letters a word needs before its Soundex code is trusted as a "sounds like"
/// signal. Below this, a code is nearly content-free — a 1-letter word codes to `<letter>000`,
/// so `u2` and `u1` (two DIFFERENT logins) would collide, and a search for one would silently
/// pull in the other. Short words still match as substrings, which is what a 1–2 character
/// query means anyway ("show me things containing this").
pub(super) const SOUNDEX_MIN_LETTERS: usize = 3;

/// [`soundex_key`], but only for a word long enough for the code to mean something (see
/// [`SOUNDEX_MIN_LETTERS`]); `None` otherwise, so the caller falls back to substring matching.
pub(super) fn phonetic_key(word: &str) -> Option<String> {
    if word.chars().filter(|c| c.is_ascii_alphabetic()).count() < SOUNDEX_MIN_LETTERS {
        return None;
    }
    soundex_key(word)
}

/// The free-text search behind the UIs' **search box**: [`matches_search`]'s case-insensitive
/// substring rule, WIDENED with a sound-alike (Soundex) match so a name typed the way it sounds
/// still finds the record (`"jonson"` finds *Johnson*, `"catherine"` finds *Katherine*).
///
/// An empty/whitespace-only query matches everything. Otherwise EVERY whitespace-separated word
/// of the query must be satisfied by the haystack — each either as a plain substring (exactly as
/// before) or by an equal [`soundex_key`] against some word of the haystack. Requiring every word
/// keeps a multi-word query narrowing rather than widening, and the substring arm makes this a
/// strict SUPERSET of the old behaviour: a search that used to hit still hits.
///
/// A query word with fewer than [`SOUNDEX_MIN_LETTERS`] letters (`"u2"`, `"2024"`, `"@"`) has no
/// trusted code, so it is matched by substring alone — digits never sound like anything, and a
/// 1–2 letter code would collide with half the vault. Soundex is a coarse ASCII heuristic, so
/// even coded words collide (`"bob"`/`"bab"`); that is the intended trade for finding a name the
/// user cannot spell, and the exact filter dropdowns remain available to narrow the list again.
pub fn matches_search_soundlike(haystack: &str, query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    let hay_lower = haystack.to_lowercase();
    // Code every haystack word once, not once per query word. The haystack splits on any
    // non-alphanumeric character, not just whitespace, so an email/handle like
    // `alice.smith@example.com` contributes the words a human hears — alice, smith, example,
    // com — instead of one unpronounceable run.
    let hay_codes: Vec<String> = haystack.split(|c: char| !c.is_alphanumeric()).filter_map(phonetic_key).collect();
    q.split_whitespace().all(|word| {
        hay_lower.contains(&word.to_lowercase())
            || phonetic_key(word).is_some_and(|qc| hay_codes.contains(&qc))
    })
}
