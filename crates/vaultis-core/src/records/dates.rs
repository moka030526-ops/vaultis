//! Calendar arithmetic on unix timestamps (no date crate): civil date ↔ days, and
//! parsing `YYYY-MM-DD` as UTC.

/// Break a unix-seconds timestamp into civil UTC `(year, month, day, hour, min,
/// sec)` using Howard Hinnant's `civil_from_days` algorithm. Negative/zero clamps
/// to the epoch. Shared by the human and filename timestamp formatters so the
/// (fiddly) calendar math lives in exactly one place.
// `pub(crate)` = visible anywhere in this crate but not to outside users.
// The return type `(i64, i64, ...)` is a *tuple*: several values bundled together.
pub fn civil_from_unix(ts: i64) -> (i64, i64, i64, i64, i64, i64) {
    // `let ts = ...` here *shadows* the parameter `ts`: a new binding reusing the name.
    // `.clamp(lo, hi)` keeps the value inside the representable 4-digit-year calendar: negatives
    // become the epoch, and anything past 9999-12-31T23:59:59Z is capped there. This guarantees
    // year ∈ [1970, 9999], so the `{:04}`/`{:02}` formatters (iso_utc, compact_utc, format_time,
    // fmt_unix) always emit fixed-width fields — a crafted/odd created_at/updated_at (up to
    // i64::MAX) can never widen the year and desync a CSV column or a timestamped filename.
    let ts = ts.clamp(0, 253_402_300_799); // 253_402_300_799 = unix_from_civil(9999, 12, 31, 23, 59, 59)
    let days = ts.div_euclid(86_400);
    let sod = ts.rem_euclid(86_400);
    let (h, m, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day, h, m, s)
}

/// Days since the Unix epoch for a civil UTC date — Howard Hinnant's
/// `days_from_civil`, the exact inverse of the `civil_from_unix` calendar math
/// above (proleptic Gregorian). `div_euclid(400)` is floored division, which is
/// what the algorithm needs for the era.
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    // March-based year: shift Jan/Feb into the previous year so the leap day is
    // the last day of the year, simplifying the day-of-year formula.
    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400; // year of era, [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 }; // month, March=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + d - 1; // day of year, [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // day of era, [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Unix seconds for a civil UTC date-time — inverse of `civil_from_unix`.
pub(crate) fn unix_from_civil(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> i64 {
    days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s
}

/// Parse a `YYYY-MM-DD` date as **UTC midnight**, returning Unix seconds.
/// Returns `None` for malformed input or an impossible calendar date (e.g.
/// `2026-02-31`), which the round-trip canonicalization check rejects. Used by
/// the `compact --history-before` cutoff.
pub fn parse_ymd_utc(s: &str) -> Option<i64> {
    // `split('-')` then `collect` into a Vec so we can require exactly 3 fields.
    let parts: Vec<&str> = s.trim().split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    // `parse::<i64>()` returns Err on non-numeric text; `.ok()?` maps that to None.
    let y: i64 = parts[0].parse().ok()?;
    let mo: i64 = parts[1].parse().ok()?;
    let d: i64 = parts[2].parse().ok()?;
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let ts = unix_from_civil(y, mo, d, 0, 0, 0);
    // Canonicalization: re-deriving the date must reproduce the input, which
    // rejects impossible dates (Feb 31, Apr 31, ...) that days_from_civil would
    // otherwise silently normalize.
    let (cy, cmo, cd, ..) = civil_from_unix(ts);
    if (cy, cmo, cd) != (y, mo, d) {
        return None;
    }
    Some(ts)
}
