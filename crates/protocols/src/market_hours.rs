//! US equity market sessions + NYSE holiday calendar, for judging tokenized-equity price
//! staleness by session instead of a fixed timeout.
//!
//! Sessions (America/New_York): overnight 20:00–04:00 (belongs to the next trading day),
//! pre-market 04:00–09:30, regular 09:30–16:00, post-market 16:00–20:00. Early-close days end the
//! regular session at 13:00 and post-market at 17:00. Weekends and NYSE holidays are closed,
//! including the overnight session leading into them (24/5 = Sun 20:00 → Fri 20:00).
//!
//! The holiday table covers 2026–2027 ([`calendar_covers`]); outside it only weekends are known.
// ponytail: hand-rolled US DST rule (2nd Sun Mar → 1st Sun Nov) instead of chrono-tz; fine until
// the rule changes by law.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc, Weekday};

/// The app maps each session to its output name (`ops::rwa::session_name`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Session {
    PreMarket,
    Regular,
    PostMarket,
    Overnight,
    Closed,
}

impl Session {
    pub fn is_open(self) -> bool {
        self != Session::Closed
    }
}

/// NYSE full-day closures, <https://www.nyse.com/markets/hours-calendars>.
const HOLIDAYS: &[(i32, u32, u32)] = &[
    (2026, 1, 1),
    (2026, 1, 19),
    (2026, 2, 16),
    (2026, 4, 3),
    (2026, 5, 25),
    (2026, 6, 19),
    (2026, 7, 3),
    (2026, 9, 7),
    (2026, 11, 26),
    (2026, 12, 25),
    (2027, 1, 1),
    (2027, 1, 18),
    (2027, 2, 15),
    (2027, 3, 26),
    (2027, 5, 31),
    (2027, 6, 18),
    (2027, 7, 5),
    (2027, 9, 6),
    (2027, 11, 25),
    (2027, 12, 24),
];

/// NYSE 1:00 pm early closes.
const EARLY_CLOSES: &[(i32, u32, u32)] = &[(2026, 11, 27), (2026, 12, 24), (2027, 11, 26)];

const CALENDAR_YEARS: std::ops::RangeInclusive<i32> = 2026..=2027;

fn in_table(table: &[(i32, u32, u32)], d: NaiveDate) -> bool {
    table
        .iter()
        .any(|&(y, m, day)| (y, m, day) == (d.year(), d.month(), d.day()))
}

/// Whether the holiday table covers `d`'s year (otherwise only weekends are known closures).
pub fn calendar_covers(d: NaiveDate) -> bool {
    CALENDAR_YEARS.contains(&d.year())
}

pub fn is_holiday(d: NaiveDate) -> bool {
    in_table(HOLIDAYS, d)
}

pub fn is_trading_day(d: NaiveDate) -> bool {
    !matches!(d.weekday(), Weekday::Sat | Weekday::Sun) && !is_holiday(d)
}

fn hm(h: u32, m: u32) -> NaiveTime {
    // Callers pass literal constants (covered by this module's tests); never panic on them.
    NaiveTime::from_hms_opt(h, m, 0).unwrap_or(NaiveTime::MIN)
}

/// (regular close, post-market end) for a trading day.
fn closes(d: NaiveDate) -> (NaiveTime, NaiveTime) {
    if in_table(EARLY_CLOSES, d) {
        (hm(13, 0), hm(17, 0))
    } else {
        (hm(16, 0), hm(20, 0))
    }
}

fn nth_sunday(year: i32, month: u32, n: u32) -> NaiveDate {
    // Month is a literal constant (3 or 11); the 1st always exists.
    let first = NaiveDate::from_ymd_opt(year, month, 1).unwrap_or_default();
    let offset = (7 - first.weekday().num_days_from_sunday()) % 7;
    first + Duration::days((offset + 7 * (n - 1)) as i64)
}

/// US Eastern DST: from 2nd Sunday of March 02:00 EST (07:00 UTC) to 1st Sunday of November
/// 02:00 EDT (06:00 UTC).
fn is_dst(t: DateTime<Utc>) -> bool {
    let y = t.year();
    let start = nth_sunday(y, 3, 2).and_time(hm(7, 0)).and_utc();
    let end = nth_sunday(y, 11, 1).and_time(hm(6, 0)).and_utc();
    t >= start && t < end
}

/// UTC → New York local time.
pub fn to_eastern(t: DateTime<Utc>) -> NaiveDateTime {
    let offset = if is_dst(t) { 4 } else { 5 };
    t.naive_utc() - Duration::hours(offset)
}

/// New York local → UTC. Only used for session boundaries, which never fall in the 01:00–02:00
/// DST ambiguity window.
fn from_eastern(local: NaiveDateTime) -> DateTime<Utc> {
    let edt = (local + Duration::hours(4)).and_utc();
    if is_dst(edt) {
        edt
    } else {
        (local + Duration::hours(5)).and_utc()
    }
}

/// Market session at instant `t`.
pub fn session_at(t: DateTime<Utc>) -> Session {
    let local = to_eastern(t);
    let (date, time) = (local.date(), local.time());
    // After 20:00 the overnight session belongs to the next trading day.
    if time >= hm(20, 0) {
        let next = date + Duration::days(1);
        return if is_trading_day(next) {
            Session::Overnight
        } else {
            Session::Closed
        };
    }
    if !is_trading_day(date) {
        return Session::Closed;
    }
    let (close, post_end) = closes(date);
    match time {
        t if t < hm(4, 0) => Session::Overnight,
        t if t < hm(9, 30) => Session::PreMarket,
        t if t < close => Session::Regular,
        t if t < post_end => Session::PostMarket,
        _ => Session::Closed, // early-close gap 17:00–20:00
    }
}

/// End of the most recent open period at or before `t` (i.e. `t` itself while open).
/// `None` only if nothing was open in the previous two weeks.
pub fn last_open_at_or_before(t: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if session_at(t).is_open() {
        return Some(t);
    }
    let today = to_eastern(t).date();
    (0..14)
        .map(|i| today - Duration::days(i))
        .filter(|d| is_trading_day(*d))
        .map(|d| from_eastern(d.and_time(closes(d).1)))
        .find(|end| *end <= t)
}

/// Session-aware staleness: the feed must have updated within `max_open_age` of the last moment
/// the market was open. On a weekend a Friday-evening price is fresh; on a Tuesday afternoon a
/// Friday price is stale.
pub fn is_stale(updated_at: DateTime<Utc>, now: DateTime<Utc>, max_open_age: Duration) -> bool {
    match last_open_at_or_before(now) {
        Some(reference) => reference - updated_at > max_open_age,
        None => now - updated_at > max_open_age,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// New York local time → UTC.
    fn ny(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        from_eastern(
            NaiveDate::from_ymd_opt(y, m, d)
                .unwrap()
                .and_time(hm(h, min)),
        )
    }

    #[test]
    fn dst_offsets() {
        // 2026: DST from Mar 8 to Nov 1.
        assert_eq!(
            nth_sunday(2026, 3, 2),
            NaiveDate::from_ymd_opt(2026, 3, 8).unwrap()
        );
        assert_eq!(
            nth_sunday(2026, 11, 1),
            NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()
        );
        let summer = ny(2026, 7, 1, 9, 30);
        assert_eq!(summer.naive_utc().time(), hm(13, 30));
        let winter = ny(2026, 12, 1, 9, 30);
        assert_eq!(winter.naive_utc().time(), hm(14, 30));
        assert_eq!(to_eastern(winter).time(), hm(9, 30));
    }

    #[test]
    fn sessions_on_a_normal_week() {
        // Wed 2026-09-23
        assert_eq!(session_at(ny(2026, 9, 23, 3, 0)), Session::Overnight);
        assert_eq!(session_at(ny(2026, 9, 23, 4, 0)), Session::PreMarket);
        assert_eq!(session_at(ny(2026, 9, 23, 9, 30)), Session::Regular);
        assert_eq!(session_at(ny(2026, 9, 23, 15, 59)), Session::Regular);
        assert_eq!(session_at(ny(2026, 9, 23, 16, 0)), Session::PostMarket);
        assert_eq!(session_at(ny(2026, 9, 23, 20, 0)), Session::Overnight);
        // Fri evening → weekend closed until Sun 20:00.
        assert_eq!(session_at(ny(2026, 9, 25, 19, 59)), Session::PostMarket);
        assert_eq!(session_at(ny(2026, 9, 25, 20, 0)), Session::Closed);
        assert_eq!(session_at(ny(2026, 9, 26, 12, 0)), Session::Closed);
        assert_eq!(session_at(ny(2026, 9, 27, 19, 59)), Session::Closed);
        assert_eq!(session_at(ny(2026, 9, 27, 20, 0)), Session::Overnight);
    }

    #[test]
    fn holidays_and_early_closes() {
        // Thanksgiving Thu 2026-11-26 closed, including the overnight into it.
        assert_eq!(session_at(ny(2026, 11, 25, 21, 0)), Session::Closed);
        assert_eq!(session_at(ny(2026, 11, 26, 12, 0)), Session::Closed);
        assert_eq!(session_at(ny(2026, 11, 26, 20, 30)), Session::Overnight);
        // Fri 2026-11-27 early close: regular to 13:00, post to 17:00, then closed.
        assert_eq!(session_at(ny(2026, 11, 27, 12, 59)), Session::Regular);
        assert_eq!(session_at(ny(2026, 11, 27, 13, 0)), Session::PostMarket);
        assert_eq!(session_at(ny(2026, 11, 27, 17, 0)), Session::Closed);
        // Good Friday 2027-03-26.
        assert_eq!(session_at(ny(2027, 3, 26, 10, 0)), Session::Closed);
        assert!(is_holiday(NaiveDate::from_ymd_opt(2027, 7, 5).unwrap()));
        assert!(calendar_covers(
            NaiveDate::from_ymd_opt(2027, 12, 31).unwrap()
        ));
        assert!(!calendar_covers(
            NaiveDate::from_ymd_opt(2028, 1, 3).unwrap()
        ));
    }

    #[test]
    fn weekend_stale_equity_feed() {
        let max = Duration::hours(1);
        let friday_close = ny(2026, 9, 25, 19, 55);
        let saturday = ny(2026, 9, 26, 12, 0);
        // Last update 5 min before Friday post-market end: fresh all weekend.
        assert!(!is_stale(friday_close, saturday, max));
        assert_eq!(
            last_open_at_or_before(saturday),
            Some(ny(2026, 9, 25, 20, 0))
        );
        // A Thursday price is stale on Saturday.
        assert!(is_stale(ny(2026, 9, 24, 15, 0), saturday, max));
        // During the session a fixed max age applies.
        let tuesday = ny(2026, 9, 29, 11, 0);
        assert!(is_stale(friday_close, tuesday, max));
        assert!(!is_stale(ny(2026, 9, 29, 10, 30), tuesday, max));
        // Thanksgiving: reference is Wednesday 20:00.
        assert!(!is_stale(
            ny(2026, 11, 25, 19, 30),
            ny(2026, 11, 26, 12, 0),
            max
        ));
    }
}
