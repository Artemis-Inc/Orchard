//! A standard 5-field cron matcher (`minute hour day-of-month month day-of-week`)
//! evaluated against UTC wall-clock minutes. Supports `*`, `*/step`, `a-b`,
//! `a-b/step`, comma lists, and single values. Day-of-week is `0..=6` with `0`
//! (and `7`) meaning Sunday. When *both* day-of-month and day-of-week are
//! restricted, a tick matches if *either* field matches (the Vixie-cron OR rule).
//!
//! Schedules are UTC — deterministic and timezone-database-free.

/// Civil time fields decomposed from unix seconds (all UTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    pub minute: i64,
    pub hour: i64,
    pub dom: i64,   // 1..=31
    pub month: i64, // 1..=12
    pub dow: i64,   // 0..=6, 0 = Sunday
}

/// Decompose unix seconds into UTC civil fields (Howard Hinnant's algorithm).
pub fn civil_from_unix(secs: i64) -> Civil {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    // day of week: 1970-01-01 was a Thursday (4); 0 = Sunday
    let dow = (days.rem_euclid(7) + 4).rem_euclid(7);
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dom = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    Civil {
        minute,
        hour,
        dom,
        month,
        dow,
    }
}

/// Does the cron `spec` fire at the UTC minute containing `unix_secs`?
pub fn matches(spec: &str, unix_secs: i64) -> Result<bool, String> {
    let c = civil_from_unix(unix_secs);
    let fields: Vec<&str> = spec.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(format!("cron expects 5 fields, got {}", fields.len()));
    }
    let min_ok = field_matches(fields[0], 0, 59, c.minute)?;
    let hour_ok = field_matches(fields[1], 0, 23, c.hour)?;
    let mon_ok = field_matches(fields[3], 1, 12, c.month)?;
    // dow: accept 7 as Sunday by normalizing the value space to 0..=7, then
    // folding 7 → 0 when testing.
    let dow_ok = dow_matches(fields[4], c.dow)?;

    let dom_restricted = fields[2].trim() != "*";
    let dow_restricted = fields[4].trim() != "*";
    let dom_ok = field_matches(fields[2], 1, 31, c.dom)?;

    let day_ok = if dom_restricted && dow_restricted {
        dom_ok || dow_ok // Vixie OR rule
    } else {
        dom_ok && dow_ok
    };

    Ok(min_ok && hour_ok && mon_ok && day_ok)
}

/// Validate a cron `spec` without evaluating it (5 well-formed fields).
pub fn validate(spec: &str) -> Result<(), String> {
    let fields: Vec<&str> = spec.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(format!("cron expects 5 fields, got {}", fields.len()));
    }
    field_matches(fields[0], 0, 59, 0)?;
    field_matches(fields[1], 0, 23, 0)?;
    field_matches(fields[2], 1, 31, 1)?;
    field_matches(fields[3], 1, 12, 1)?;
    dow_matches(fields[4], 0)?;
    Ok(())
}

fn dow_matches(field: &str, value: i64) -> Result<bool, String> {
    // Day-of-week space is 0..=7 (both 0 and 7 = Sunday). Test the given value
    // and its Sunday alias against the parsed set.
    let set = parse_field(field, 0, 7)?;
    Ok(set_contains(&set, value) || (value == 0 && set_contains(&set, 7)))
}

fn field_matches(field: &str, min: i64, max: i64, value: i64) -> Result<bool, String> {
    let set = parse_field(field, min, max)?;
    Ok(set_contains(&set, value))
}

/// A parsed field: either "any" or an explicit sorted set of allowed values.
enum FieldSet {
    Any,
    Values(Vec<i64>),
}

fn set_contains(set: &FieldSet, value: i64) -> bool {
    match set {
        FieldSet::Any => true,
        FieldSet::Values(v) => v.binary_search(&value).is_ok(),
    }
}

fn parse_field(field: &str, min: i64, max: i64) -> Result<FieldSet, String> {
    let field = field.trim();
    if field.is_empty() {
        return Err("empty cron field".into());
    }
    if field == "*" {
        return Ok(FieldSet::Any);
    }
    let mut values: Vec<i64> = Vec::new();
    for part in field.split(',') {
        let part = part.trim();
        // split off an optional /step
        let (range_str, step) = match part.split_once('/') {
            Some((r, s)) => {
                let step: i64 = s
                    .trim()
                    .parse()
                    .map_err(|_| format!("bad cron step '{s}'"))?;
                if step <= 0 {
                    return Err(format!("cron step must be positive: '{part}'"));
                }
                (r.trim(), step)
            }
            None => (part, 1),
        };
        let (lo, hi) = if range_str == "*" {
            (min, max)
        } else if let Some((a, b)) = range_str.split_once('-') {
            let a: i64 = a
                .trim()
                .parse()
                .map_err(|_| format!("bad cron range '{range_str}'"))?;
            let b: i64 = b
                .trim()
                .parse()
                .map_err(|_| format!("bad cron range '{range_str}'"))?;
            (a, b)
        } else {
            let n: i64 = range_str
                .trim()
                .parse()
                .map_err(|_| format!("bad cron value '{range_str}'"))?;
            // a bare value with a step (e.g. `5/10`) means "from 5 to max step 10"
            if step > 1 {
                (n, max)
            } else {
                (n, n)
            }
        };
        if lo < min || hi > max || lo > hi {
            return Err(format!("cron value out of range [{min},{max}]: '{part}'"));
        }
        let mut v = lo;
        while v <= hi {
            values.push(v);
            v += step;
        }
    }
    values.sort_unstable();
    values.dedup();
    Ok(FieldSet::Values(values))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2021-01-01 00:00:00 UTC = 1609459200 (a Friday, dow=5)
    const NY2021: i64 = 1_609_459_200;

    #[test]
    fn civil_decode() {
        let c = civil_from_unix(NY2021);
        assert_eq!(
            c,
            Civil {
                minute: 0,
                hour: 0,
                dom: 1,
                month: 1,
                dow: 5
            }
        );
    }

    #[test]
    fn every_minute() {
        assert!(matches("* * * * *", NY2021).unwrap());
    }

    #[test]
    fn specific_time() {
        assert!(matches("0 0 1 1 *", NY2021).unwrap());
        assert!(!matches("30 0 1 1 *", NY2021).unwrap());
    }

    #[test]
    fn step_and_list() {
        // minute 0 matches */15 and 0,30
        assert!(matches("*/15 * * * *", NY2021).unwrap());
        assert!(matches("0,30 * * * *", NY2021).unwrap());
        assert!(!matches("*/15 * * * *", NY2021 + 300).unwrap()); // 00:05 not divisible by 15
        assert!(matches("*/15 * * * *", NY2021 + 900).unwrap()); // 00:15
    }

    #[test]
    fn range() {
        assert!(matches("0 0-6 * * *", NY2021).unwrap()); // hour 0 in 0-6
        assert!(!matches("0 8-17 * * *", NY2021).unwrap());
    }

    #[test]
    fn dow_sunday_aliases() {
        // 2021-01-03 was a Sunday (dow=0); unix 1609632000
        let sunday = 1_609_632_000;
        assert_eq!(civil_from_unix(sunday).dow, 0);
        assert!(matches("0 0 * * 0", sunday).unwrap());
        assert!(matches("0 0 * * 7", sunday).unwrap()); // 7 == Sunday
        assert!(!matches("0 0 * * 1", sunday).unwrap());
    }

    #[test]
    fn dom_dow_or_rule() {
        // Friday 2021-01-01, dom=1. With both restricted, OR: match dom=1 even
        // though dow != Monday.
        assert!(matches("0 0 1 * 1", NY2021).unwrap());
        // dow matches (Friday=5) even though dom != 15
        assert!(matches("0 0 15 * 5", NY2021).unwrap());
        // neither matches
        assert!(!matches("0 0 15 * 1", NY2021).unwrap());
    }

    #[test]
    fn bad_specs() {
        assert!(matches("* * * *", 0).is_err()); // 4 fields
        assert!(matches("60 * * * *", 0).is_err()); // minute out of range
        assert!(matches("*/0 * * * *", 0).is_err()); // zero step
        assert!(validate("0 9 * * 1-5").is_ok());
    }
}
