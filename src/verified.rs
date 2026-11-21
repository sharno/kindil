//! The integer core, verified with Verus: everything from the sun's events,
//! as whole seconds from their date's 00:00 UTC, to the times shown. The
//! calendar (day numbers, day of the year, the solstice), the rules that
//! choose each time (and say when a rule stood in for the sun), the order
//! guard, offsets, rounding, the night's middle and
//! last third, and the prayer of a moment.
//!
//! Inside `verus!` each function has its specification (`requires`,
//! `ensures`) and proof; Verus proves them for every input (VERIFICATION.md).
//! A normal build erases the specs and proofs and compiles the rest, the
//! same functions, as plain Rust. Callers outside must meet the
//! `requires`: the float layer's conversions are proven to by Kani.
//!
//! No `crate::` paths here: Verus checks this file as a crate of its own
//! (`verus src/verified.rs --crate-type=lib`).
// The specs use constants and accessors a normal build, which erases
// them, finds unused.
#![allow(dead_code)]
// Verus doesn't model RangeInclusive::contains or compound assignment.
#![allow(
    clippy::assign_op_pattern,
    clippy::manual_range_contains,
    clippy::match_like_matches_macro
)]

use verus_builtin_macros::verus;
#[cfg(verus_keep_ghost)]
#[allow(unused_imports)]
use vstd::prelude::*;

verus! {

// ---------------------------------------------------------------- calendar

pub const MIN_YEAR: i32 = 999;
pub const MAX_YEAR: i32 = 3001;
pub const MIN_DAY_NUMBER: i64 = -360_000;
pub const MAX_DAY_NUMBER: i64 = 380_000;
/// Days from 0001-01-01 to 1970-01-01.
pub const EPOCH_DAYS: i64 = 719_162;

pub open spec fn leap(y: int) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

pub open spec fn days_in_year(y: int) -> int {
    if leap(y) { 366 } else { 365 }
}

pub open spec fn days_in_month(y: int, m: int) -> int {
    if m == 2 {
        if leap(y) { 29 } else { 28 }
    } else if m == 4 || m == 6 || m == 9 || m == 11 {
        30
    } else {
        31
    }
}

pub open spec fn valid_date(y: int, m: int, d: int) -> bool {
    MIN_YEAR <= y <= MAX_YEAR && 1 <= m <= 12 && 1 <= d <= days_in_month(y, m)
}

pub open spec fn days_before_month(y: int, m: int) -> int
    decreases m,
{
    if m <= 1 { 0 } else { days_before_month(y, m - 1) + days_in_month(y, m - 1) }
}

pub open spec fn day_of_year(y: int, m: int, d: int) -> int {
    days_before_month(y, m) + d
}

pub open spec fn days_before_year(y: int) -> int {
    365 * (y - 1) + (y - 1) / 4 - (y - 1) / 100 + (y - 1) / 400
}

/// Days since 1970-01-01.
pub open spec fn day_number(y: int, m: int, d: int) -> int {
    days_before_year(y) + day_of_year(y, m, d) - 1 - EPOCH_DAYS
}

pub open spec fn next_date(y: int, m: int, d: int) -> (int, int, int) {
    if d < days_in_month(y, m) {
        (y, m, d + 1)
    } else if m < 12 {
        (y, m + 1, 1)
    } else {
        (y + 1, 1, 1)
    }
}

proof fn lemma_days_before_month_values(y: int)
    ensures
        days_before_month(y, 1) == 0,
        days_before_month(y, 2) == 31,
        days_before_month(y, 3) == 59 + if leap(y) { 1int } else { 0 },
        days_before_month(y, 4) == 90 + if leap(y) { 1int } else { 0 },
        days_before_month(y, 5) == 120 + if leap(y) { 1int } else { 0 },
        days_before_month(y, 6) == 151 + if leap(y) { 1int } else { 0 },
        days_before_month(y, 7) == 181 + if leap(y) { 1int } else { 0 },
        days_before_month(y, 8) == 212 + if leap(y) { 1int } else { 0 },
        days_before_month(y, 9) == 243 + if leap(y) { 1int } else { 0 },
        days_before_month(y, 10) == 273 + if leap(y) { 1int } else { 0 },
        days_before_month(y, 11) == 304 + if leap(y) { 1int } else { 0 },
        days_before_month(y, 12) == 334 + if leap(y) { 1int } else { 0 },
        days_before_month(y, 13) == days_in_year(y),
{
    reveal_with_fuel(days_before_month, 13);
}

proof fn lemma_days_before_year_step(y: int)
    requires
        y >= 1,
    ensures
        days_before_year(y + 1) == days_before_year(y) + days_in_year(y),
{
}

/// The day after a date is a date, and its day number is one more: the
/// day numbers count the days.
pub proof fn lemma_day_number_successor(y: int, m: int, d: int)
    requires
        valid_date(y, m, d),
        y < MAX_YEAR || m < 12 || d < days_in_month(y, m),
    ensures
        valid_date(next_date(y, m, d).0, next_date(y, m, d).1, next_date(y, m, d).2),
        day_number(next_date(y, m, d).0, next_date(y, m, d).1, next_date(y, m, d).2)
            == day_number(y, m, d) + 1,
{
    lemma_days_before_month_values(y);
    lemma_days_before_month_values(y + 1);
    lemma_days_before_year_step(y);
}

/// 1970-01-01 is day 0.
pub proof fn lemma_epoch()
    ensures
        day_number(1970, 1, 1) == 0,
{
    lemma_days_before_month_values(1970);
}

pub fn is_leap(y: i32) -> (r: bool)
    requires
        1 <= y <= 9999,
    ensures
        r == leap(y as int),
{
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

pub fn month_length(y: i32, m: u8) -> (r: u8)
    requires
        1 <= y <= 9999,
        1 <= m <= 12,
    ensures
        r as int == days_in_month(y as int, m as int),
{
    if m == 2 {
        if is_leap(y) { 29 } else { 28 }
    } else if m == 4 || m == 6 || m == 9 || m == 11 {
        30
    } else {
        31
    }
}

/// The day of the year, 1 on 1 January.
pub fn day_of_year_of(y: i32, m: u8, d: u8) -> (r: u16)
    requires
        valid_date(y as int, m as int, d as int),
    ensures
        r as int == day_of_year(y as int, m as int, d as int),
        1 <= r <= days_in_year(y as int),
{
    proof {
        lemma_days_before_month_values(y as int);
    }
    let before: u16 = if m == 1 {
        0
    } else if m == 2 {
        31
    } else if m == 3 {
        59
    } else if m == 4 {
        90
    } else if m == 5 {
        120
    } else if m == 6 {
        151
    } else if m == 7 {
        181
    } else if m == 8 {
        212
    } else if m == 9 {
        243
    } else if m == 10 {
        273
    } else if m == 11 {
        304
    } else {
        334
    };
    let extra: u16 = if m > 2 && is_leap(y) { 1 } else { 0 };
    before + extra + d as u16
}

/// Days since 1970-01-01 (negative before).
pub fn day_number_of(y: i32, m: u8, d: u8) -> (r: i64)
    requires
        valid_date(y as int, m as int, d as int),
    ensures
        r as int == day_number(y as int, m as int, d as int),
        MIN_DAY_NUMBER <= r <= MAX_DAY_NUMBER,
{
    let years = (y - 1) as i64;
    let doy = day_of_year_of(y, m, d) as i64;
    proof {
        let x = years as int;
        assert(0 <= x / 100 <= x / 4 <= 750);
        assert(0 <= x / 400 <= 8);
    }
    365 * years + years / 4 - years / 100 + years / 400 + doy - 1 - EPOCH_DAYS
}

pub open spec fn previous_date(y: int, m: int, d: int) -> (int, int, int) {
    if d > 1 {
        (y, m, d - 1)
    } else if m > 1 {
        (y, m - 1, days_in_month(y, m - 1))
    } else {
        (y - 1, 12, 31)
    }
}

/// A date of the proleptic Gregorian calendar, years 999 to 3001: always
/// valid (its type invariant).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Civil {
    year: i32,
    month: u8,
    day: u8,
}

impl Civil {
    #[verifier::type_invariant]
    pub closed spec fn valid(self) -> bool {
        valid_date(self.year as int, self.month as int, self.day as int)
    }

    pub closed spec fn view_ymd(self) -> (int, int, int) {
        (self.year as int, self.month as int, self.day as int)
    }

    /// The date, if it is one in years 999 to 3001.
    pub fn new(year: i32, month: u8, day: u8) -> (r: Option<Civil>)
        ensures
            r.is_some() <==> valid_date(year as int, month as int, day as int),
            r matches Some(c) ==> c.view_ymd() == (year as int, month as int, day as int),
    {
        if year < MIN_YEAR || year > MAX_YEAR || month < 1 || month > 12 || day < 1 {
            return None;
        }
        if day > month_length(year, month) {
            return None;
        }
        Some(Civil { year, month, day })
    }

    pub fn year(&self) -> (r: i32)
        ensures
            r as int == self.view_ymd().0,
            MIN_YEAR <= r <= MAX_YEAR,
    {
        proof { use_type_invariant(self); }
        self.year
    }

    pub fn month(&self) -> (r: u8)
        ensures
            r as int == self.view_ymd().1,
            1 <= r <= 12,
    {
        proof { use_type_invariant(self); }
        self.month
    }

    pub fn day(&self) -> (r: u8)
        ensures
            r as int == self.view_ymd().2,
            1 <= r <= 31,
    {
        proof { use_type_invariant(self); }
        self.day
    }

    /// Days since 1970-01-01.
    pub fn number(&self) -> (r: i64)
        ensures
            r as int == day_number(self.view_ymd().0, self.view_ymd().1, self.view_ymd().2),
            MIN_DAY_NUMBER <= r <= MAX_DAY_NUMBER,
    {
        proof { use_type_invariant(self); }
        day_number_of(self.year, self.month, self.day)
    }

    /// 1 on 1 January.
    pub fn day_of_year(&self) -> (r: u16)
        ensures
            1 <= r <= days_in_year(self.view_ymd().0),
    {
        proof { use_type_invariant(self); }
        day_of_year_of(self.year, self.month, self.day)
    }

    /// The next day, a day number later; None after 3001.
    pub fn next(&self) -> (r: Option<Civil>)
        ensures
            r matches Some(c) ==> c.view_ymd() == next_date(
                self.view_ymd().0,
                self.view_ymd().1,
                self.view_ymd().2,
            ) && day_number(c.view_ymd().0, c.view_ymd().1, c.view_ymd().2) == day_number(
                self.view_ymd().0,
                self.view_ymd().1,
                self.view_ymd().2,
            ) + 1,
            r.is_none() <==> self.view_ymd().0 == MAX_YEAR && self.view_ymd().1 == 12
                && self.view_ymd().2 == 31,
    {
        proof { use_type_invariant(self); }
        let (y, m, d) = (self.year, self.month, self.day);
        if d < month_length(y, m) {
            Some(Civil { year: y, month: m, day: d + 1 })
        } else if m < 12 {
            Some(Civil { year: y, month: m + 1, day: 1 })
        } else if y < MAX_YEAR {
            proof { lemma_day_number_successor(y as int, m as int, d as int); }
            Some(Civil { year: y + 1, month: 1, day: 1 })
        } else {
            None
        }
    }

    /// The day before, a day number earlier; None before 999.
    pub fn previous(&self) -> (r: Option<Civil>)
        ensures
            r matches Some(c) ==> c.view_ymd() == previous_date(
                self.view_ymd().0,
                self.view_ymd().1,
                self.view_ymd().2,
            ) && day_number(c.view_ymd().0, c.view_ymd().1, c.view_ymd().2) + 1 == day_number(
                self.view_ymd().0,
                self.view_ymd().1,
                self.view_ymd().2,
            ),
            r.is_none() <==> self.view_ymd().0 == MIN_YEAR && self.view_ymd().1 == 1
                && self.view_ymd().2 == 1,
    {
        proof { use_type_invariant(self); }
        let (y, m, d) = (self.year, self.month, self.day);
        if d > 1 {
            Some(Civil { year: y, month: m, day: d - 1 })
        } else if m > 1 {
            let length = month_length(y, m - 1);
            proof { lemma_day_number_successor(y as int, m as int - 1, length as int); }
            Some(Civil { year: y, month: m - 1, day: length })
        } else if y > MIN_YEAR {
            proof { lemma_day_number_successor(y as int - 1, 12, 31); }
            Some(Civil { year: y - 1, month: 12, day: 31 })
        } else {
            None
        }
    }
}

pub open spec fn days_since_solstice_spec(doy: int, y: int, north: bool) -> int {
    let offset = if north {
        -10int
    } else if leap(y) {
        173int
    } else {
        172int
    };
    (doy - offset) % days_in_year(y)
}

/// Days since the winter solstice: the northern one about 10 days before
/// the year begins, the southern one day 172 (173 in a leap year).
pub fn days_since_solstice(doy: u16, y: i32, north: bool) -> (r: u16)
    requires
        1 <= y <= 9999,
        1 <= doy <= days_in_year(y as int),
    ensures
        r as int == days_since_solstice_spec(doy as int, y as int, north),
        r < days_in_year(y as int),
{
    let leap = is_leap(y);
    let length: i32 = if leap { 366 } else { 365 };
    let offset: i32 = if north {
        -10
    } else if leap {
        173
    } else {
        172
    };
    // In [-173, 376]: at most a year either way.
    let x = doy as i32 - offset;
    if x < 0 {
        (x + length) as u16
    } else if x >= length {
        (x - length) as u16
    } else {
        x as u16
    }
}

// ---------------------------------------------------------------- rounding

/// How a time is put on a minute.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Round {
    /// To the nearest minute, half a minute up.
    Nearest,
    /// To the next minute; a whole minute stays.
    Up,
    /// Not at all.
    Exact,
}

pub open spec fn round_spec(s: int, mode: Round) -> int {
    match mode {
        Round::Nearest => ((s + 30) / 60) * 60,
        Round::Up => -((-s) / 60) * 60,
        Round::Exact => s,
    }
}

pub const ROUND_BOUND: i64 = 1_000_000_000_000;

fn floor_div_60(a: i64) -> (q: i64)
    requires
        -ROUND_BOUND <= a <= ROUND_BOUND,
    ensures
        q as int == a as int / 60,
{
    let q = a / 60;
    if a % 60 < 0 { q - 1 } else { q }
}

/// `s` seconds put on a minute (or not), exactly as `round_spec`.
pub fn round(s: i64, mode: Round) -> (r: i64)
    requires
        -ROUND_BOUND + 60 <= s <= ROUND_BOUND - 60,
    ensures
        r as int == round_spec(s as int, mode),
{
    match mode {
        Round::Nearest => floor_div_60(s + 30) * 60,
        Round::Up => -floor_div_60(-s) * 60,
        Round::Exact => s,
    }
}

/// Rounding moves a time by less than a minute, the right way, onto a
/// minute (but for Exact).
pub proof fn lemma_round_bounds(s: int, mode: Round)
    ensures
        mode == Round::Nearest ==> s - 29 <= round_spec(s, mode) <= s + 30,
        mode == Round::Up ==> s <= round_spec(s, mode) <= s + 59,
        mode == Round::Exact ==> round_spec(s, mode) == s,
        mode != Round::Exact ==> round_spec(s, mode) % 60 == 0,
{
}

/// Rounding twice is rounding once.
pub proof fn lemma_round_idempotent(s: int, mode: Round)
    ensures
        round_spec(round_spec(s, mode), mode) == round_spec(s, mode),
{
}

/// Rounding keeps order.
pub proof fn lemma_round_monotone(a: int, b: int, mode: Round)
    requires
        a <= b,
    ensures
        round_spec(a, mode) <= round_spec(b, mode),
{
}

/// A time already on a minute stays there.
pub proof fn lemma_round_minute(s: int, mode: Round)
    requires
        s % 60 == 0,
    ensures
        round_spec(s, mode) == s,
{
}


// ------------------------------------------------------------ moments

pub const DAY: i64 = 86_400;
/// The least time between one prayer and the next (Adhan's).
pub const MIN_GAP: i64 = 300;
/// The sun's events are within this of their date's 00:00 UTC (the float
/// layer's conversion checks it: Kani).
pub const EVENT_BOUND: i64 = 4 * DAY;
/// A resolved day's times: events, and shares of a night, within this.
pub const DAY_BOUND: i64 = 16 * DAY;
/// Method and user offsets together, in minutes.
pub const MAX_OFFSET_MINUTES: i32 = 1440;
pub const ADJUSTED_BOUND: i64 = DAY_BOUND + 1440 * 60;
/// The Moonsighting Committee's seasonal twilights are at most this long.
pub const SEASON_BOUND: i64 = 6 * 3600;
/// Absolute seconds (since 1970-01-01 00:00 UTC) the core makes.
pub const MAX_SECONDS: i64 = 40_000_000_000;

/// The time `offset` seconds from 00:00 UTC on day `day`.
pub fn absolute(day: i64, offset: i64) -> (r: i64)
    requires
        MIN_DAY_NUMBER <= day <= MAX_DAY_NUMBER,
        -ADJUSTED_BOUND - 60 <= offset <= ADJUSTED_BOUND + 60,
    ensures
        r == day * DAY + offset,
        -MAX_SECONDS <= r <= MAX_SECONDS,
{
    day * DAY + offset
}

/// A time as its day and the second of that day: the day rolls over at
/// 24:00.
pub fn day_and_second(t: i64) -> (r: (i64, i64))
    requires
        -MAX_SECONDS <= t <= MAX_SECONDS,
    ensures
        r.0 * DAY + r.1 == t,
        0 <= r.1 < DAY,
        r.0 == t as int / DAY as int,
{
    let q = t / DAY;
    let m = t % DAY;
    if m < 0 { (q - 1, m + DAY) } else { (q, m) }
}

/// Minutes added to a time, exactly.
pub fn add_minutes(offset: i64, minutes: i32) -> (r: i64)
    requires
        -DAY_BOUND <= offset <= DAY_BOUND,
        -MAX_OFFSET_MINUTES <= minutes <= MAX_OFFSET_MINUTES,
    ensures
        r == offset + 60 * minutes,
        -ADJUSTED_BOUND <= r <= ADJUSTED_BOUND,
{
    offset + 60 * minutes as i64
}

// ------------------------------------------------------------ a day's times

/// Why a time is missing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Missing {
    /// No sunrise (Sunrise, and Fajr, which is bound to it).
    Sunrise,
    /// No sunset (Maghrib, and Isha).
    Sunset,
    /// No sunrise the next day: no length of the night (Fajr and Isha by
    /// angle).
    NextSunrise,
    /// The sun doesn't reach the Asr shadow.
    Asr,
    /// The day's times aren't in order five minutes apart.
    Order,
}

/// The rule that gave a time in place of the sun's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rule {
    /// Fajr no earlier, Isha no later, than a share of the night.
    NightShare,
    /// The Moonsighting Committee's seasonal twilight.
    Season,
    /// The Moonsighting Committee's seventh of the night (above 55°).
    Seventh,
    /// Maghrib at sunset: the Maghrib angle wasn't reached, or crowded Isha.
    Sunset,
}

/// One of a day's times.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    /// As the method says (the sun at its angle, or an interval).
    At(i64),
    /// From a rule, in place of the sun's.
    Fallback(i64, Rule),
    Missing(Missing),
}

pub open spec fn timed(s: Slot) -> bool {
    match s {
        Slot::At(_) | Slot::Fallback(_, _) => true,
        Slot::Missing(_) => false,
    }
}

pub open spec fn time_of(s: Slot) -> int {
    match s {
        Slot::At(t) | Slot::Fallback(t, _) => t as int,
        Slot::Missing(_) => 0,
    }
}

pub open spec fn within(s: Slot, bound: int) -> bool {
    timed(s) ==> -bound <= time_of(s) <= bound
}

/// A share of the night: `numerator / denominator`, at most a half.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Share {
    pub numerator: i64,
    pub denominator: i64,
}

pub open spec fn valid_share(s: Share) -> bool {
    0 <= s.numerator && 1 <= s.denominator <= 6000 && 2 * s.numerator <= s.denominator
}

/// How Fajr's earliest and Isha's latest times are bounded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bound {
    /// By a share of the night (the high-latitude rule).
    Share { fajr_share: Share, isha_share: Share },
    /// By the Moonsighting Committee's seasonal twilights, `fajr` seconds
    /// before sunrise and `isha` after sunset; above 55° (`seventh`), with
    /// a seventh of the night in place of the angles.
    Seasons { fajr_season: i64, isha_season: i64, seventh: bool },
}

pub open spec fn valid_bound(b: Bound) -> bool {
    match b {
        Bound::Share { fajr_share, isha_share } => valid_share(fajr_share) && valid_share(isha_share),
        Bound::Seasons { fajr_season, isha_season, seventh: _ } => 0 <= fajr_season <= SEASON_BOUND
            && 0 <= isha_season <= SEASON_BOUND,
    }
}

/// When Isha is: at the sun's angle, or minutes after sunset.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IshaBy {
    Angle,
    Interval(i64),
}

pub open spec fn valid_isha_by(i: IshaBy) -> bool {
    match i {
        IshaBy::Angle => true,
        IshaBy::Interval(minutes) => 5 <= minutes <= 240,
    }
}

/// When Maghrib is: at sunset, or when the sun is at an angle (if it gets
/// there: None when it doesn't).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MaghribBy {
    Sunset,
    Angle(Option<i64>),
}

/// The sun's events of a day, in seconds from its 00:00 UTC: sunrise,
/// transit, sunset, the next sunrise, and when the sun is at the method's
/// Fajr and Isha angles and the Asr shadow (None when it never is).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Events {
    pub sunrise: Option<i64>,
    pub transit: i64,
    pub sunset: Option<i64>,
    pub next_sunrise: Option<i64>,
    pub fajr: Option<i64>,
    pub isha: Option<i64>,
    pub maghrib: MaghribBy,
    pub asr: Option<i64>,
}

pub open spec fn near(o: Option<i64>) -> bool {
    match o {
        Some(t) => -EVENT_BOUND <= t <= EVENT_BOUND,
        None => true,
    }
}

pub open spec fn valid_events(e: Events) -> bool {
    near(e.sunrise) && -EVENT_BOUND <= e.transit <= EVENT_BOUND && near(e.sunset) && near(
        e.next_sunrise,
    ) && near(e.fajr) && near(e.isha) && near(e.asr) && match e.maghrib {
        MaghribBy::Sunset => true,
        MaghribBy::Angle(a) => near(a),
    }
}

/// floor(n * share): a share of a night of `n` seconds, n >= 0.
pub open spec fn share_of(n: int, s: Share) -> int {
    (n * s.numerator as int) / s.denominator as int
}

/// ceil(n * share): Fajr's side, where Adhan takes the share from sunrise
/// and then floors the time.
pub open spec fn share_up(n: int, s: Share) -> int {
    (n * s.numerator as int + s.denominator as int - 1) / s.denominator as int
}

fn share_seconds_up(n: i64, s: Share) -> (r: i64)
    requires
        0 <= n <= 2 * EVENT_BOUND,
        valid_share(s),
    ensures
        r == share_up(n as int, s),
        0 <= r <= n,
{
    proof {
        let (ni, a, d) = (n as int, s.numerator as int, s.denominator as int);
        assert(0 <= ni * a <= ni * 3000) by (nonlinear_arith)
            requires
                0 <= ni,
                0 <= a <= 3000,
        ;
        assert((ni * a + d - 1) / d <= ni) by (nonlinear_arith)
            requires
                0 <= ni,
                0 <= a,
                1 <= d,
                2 * a <= d,
        ;
        assert(0 <= (ni * a + d - 1) / d) by (nonlinear_arith)
            requires
                0 <= ni * a,
                1 <= d,
        ;
    }
    (n * s.numerator + s.denominator - 1) / s.denominator
}

fn share_seconds(n: i64, s: Share) -> (r: i64)
    requires
        0 <= n <= 2 * EVENT_BOUND,
        valid_share(s),
    ensures
        r == share_of(n as int, s),
        0 <= r <= n,
        2 * r <= n,
{
    proof {
        let (ni, a, d) = (n as int, s.numerator as int, s.denominator as int);
        assert(0 <= ni * a <= ni * 3000) by (nonlinear_arith)
            requires
                0 <= ni,
                0 <= a <= 3000,
        ;
        assert(2 * ((ni * a) / d) <= ni) by (nonlinear_arith)
            requires
                0 <= ni,
                0 <= a,
                1 <= d,
                2 * a <= d,
        ;
        assert(0 <= (ni * a) / d) by (nonlinear_arith)
            requires
                0 <= ni * a,
                1 <= d,
        ;
    }
    n * s.numerator / s.denominator
}

/// The night's length, from sunset to the next sunrise, if both are and
/// it isn't negative.
pub open spec fn night_spec(e: Events) -> Option<int> {
    match (e.sunset, e.next_sunrise) {
        (Some(s), Some(n)) => if n >= s { Some(n - s) } else { None },
        _ => None,
    }
}

fn night_of(e: &Events) -> (r: Option<i64>)
    requires
        valid_events(*e),
    ensures
        match r {
            Some(n) => night_spec(*e) == Some(n as int) && 0 <= n <= 2 * EVENT_BOUND,
            None => night_spec(*e).is_none(),
        },
{
    match (e.sunset, e.next_sunrise) {
        (Some(s), Some(n)) => if n >= s { Some(n - s) } else { None },
        _ => None,
    }
}

/// Fajr: the sun at its angle (or the seventh of the night, above 55° for
/// the Moonsighting Committee), unless that is earlier than the bound or
/// within five minutes of sunrise; then the bound.
pub open spec fn fajr_spec(e: Events, b: Bound) -> Slot {
    match (e.sunrise, night_spec(e)) {
        (None, _) => Slot::Missing(Missing::Sunrise),
        (Some(_), None) => Slot::Missing(Missing::NextSunrise),
        (Some(rise), Some(night)) => {
            let (safe, rule, candidate) = match b {
                Bound::Share { fajr_share: fajr, isha_share: _ } => (
                    rise - share_up(night, fajr),
                    Rule::NightShare,
                    match e.fajr { Some(t) => Slot::At(t), None => Slot::Missing(Missing::Order) },
                ),
                Bound::Seasons { fajr_season: fajr, isha_season: _, seventh } => (
                    rise - fajr,
                    Rule::Season,
                    if seventh {
                        Slot::Fallback((rise - (night + 6) / 7) as i64, Rule::Seventh)
                    } else {
                        match e.fajr { Some(t) => Slot::At(t), None => Slot::Missing(Missing::Order) }
                    },
                ),
            };
            if timed(candidate) && time_of(candidate) >= safe && rise - time_of(candidate) >= MIN_GAP {
                candidate
            } else {
                Slot::Fallback(safe as i64, rule)
            }
        },
    }
}

/// Isha: minutes after sunset, or the sun at its angle (or the seventh of
/// the night), unless that is later than the bound or within five minutes
/// of sunset; then the bound.
pub open spec fn isha_spec(e: Events, b: Bound, by: IshaBy) -> Slot {
    match (e.sunset, by) {
        (None, _) => Slot::Missing(Missing::Sunset),
        (Some(set), IshaBy::Interval(minutes)) => Slot::At((set + 60 * minutes) as i64),
        (Some(set), IshaBy::Angle) => match night_spec(e) {
            None => Slot::Missing(Missing::NextSunrise),
            Some(night) => {
                let (safe, rule, candidate) = match b {
                    Bound::Share { fajr_share: _, isha_share: isha } => (
                        set + share_of(night, isha),
                        Rule::NightShare,
                        match e.isha { Some(t) => Slot::At(t), None => Slot::Missing(Missing::Order) },
                    ),
                    Bound::Seasons { fajr_season: _, isha_season: isha, seventh } => (
                        set + isha,
                        Rule::Season,
                        if seventh {
                            Slot::Fallback((set + night / 7) as i64, Rule::Seventh)
                        } else {
                            match e.isha { Some(t) => Slot::At(t), None => Slot::Missing(Missing::Order) }
                        },
                    ),
                };
                if timed(candidate) && time_of(candidate) <= safe && time_of(candidate) - set >= MIN_GAP {
                    candidate
                } else {
                    Slot::Fallback(safe as i64, rule)
                }
            },
        },
    }
}

/// Maghrib: at sunset, or at the sun's angle if that is after sunset and
/// five minutes before Isha (or there is no Isha); else at sunset.
pub open spec fn maghrib_spec(e: Events, isha: Slot) -> Slot {
    match (e.sunset, e.maghrib) {
        (None, _) => Slot::Missing(Missing::Sunset),
        (Some(set), MaghribBy::Sunset) => Slot::At(set),
        (Some(set), MaghribBy::Angle(None)) => Slot::Fallback(set, Rule::Sunset),
        (Some(set), MaghribBy::Angle(Some(t))) => if t > set && (!timed(isha) || time_of(isha) - t
            >= MIN_GAP) {
            Slot::At(t)
        } else {
            Slot::Fallback(set, Rule::Sunset)
        },
    }
}

fn candidate_at(o: Option<i64>) -> (r: Slot)
    ensures
        r == match o { Some(t) => Slot::At(t), None => Slot::Missing(Missing::Order) },
{
    match o {
        Some(t) => Slot::At(t),
        None => Slot::Missing(Missing::Order),
    }
}

fn fajr_of(e: &Events, b: &Bound) -> (r: Slot)
    requires
        valid_events(*e),
        valid_bound(*b),
    ensures
        r == fajr_spec(*e, *b),
        within(r, DAY_BOUND as int),
{
    match (e.sunrise, night_of(e)) {
        (None, _) => Slot::Missing(Missing::Sunrise),
        (Some(_), None) => Slot::Missing(Missing::NextSunrise),
        (Some(rise), Some(night)) => {
            let (safe, rule, candidate) = match *b {
                Bound::Share { fajr_share: fajr, isha_share: _ } => (
                    rise - share_seconds_up(night, fajr),
                    Rule::NightShare,
                    candidate_at(e.fajr),
                ),
                Bound::Seasons { fajr_season: fajr, isha_season: _, seventh } => (
                    rise - fajr,
                    Rule::Season,
                    if seventh {
                        Slot::Fallback(rise - (night + 6) / 7, Rule::Seventh)
                    } else {
                        candidate_at(e.fajr)
                    },
                ),
            };
            let keep = match candidate {
                Slot::At(t) | Slot::Fallback(t, _) => t >= safe && rise - t >= MIN_GAP,
                Slot::Missing(_) => false,
            };
            if keep {
                candidate
            } else {
                Slot::Fallback(safe, rule)
            }
        },
    }
}

fn isha_of(e: &Events, b: &Bound, by: IshaBy) -> (r: Slot)
    requires
        valid_events(*e),
        valid_bound(*b),
        valid_isha_by(by),
    ensures
        r == isha_spec(*e, *b, by),
        within(r, DAY_BOUND as int),
{
    match (e.sunset, by) {
        (None, _) => Slot::Missing(Missing::Sunset),
        (Some(set), IshaBy::Interval(minutes)) => Slot::At(set + 60 * minutes),
        (Some(set), IshaBy::Angle) => match night_of(e) {
            None => Slot::Missing(Missing::NextSunrise),
            Some(night) => {
                let (safe, rule, candidate) = match *b {
                    Bound::Share { fajr_share: _, isha_share: isha } => (
                        set + share_seconds(night, isha),
                        Rule::NightShare,
                        candidate_at(e.isha),
                    ),
                    Bound::Seasons { fajr_season: _, isha_season: isha, seventh } => (
                        set + isha,
                        Rule::Season,
                        if seventh {
                            Slot::Fallback(set + night / 7, Rule::Seventh)
                        } else {
                            candidate_at(e.isha)
                        },
                    ),
                };
                let keep = match candidate {
                    Slot::At(t) | Slot::Fallback(t, _) => t <= safe && t - set >= MIN_GAP,
                    Slot::Missing(_) => false,
                };
                if keep {
                    candidate
                } else {
                    Slot::Fallback(safe, rule)
                }
            },
        },
    }
}

fn maghrib_of(e: &Events, isha: Slot) -> (r: Slot)
    requires
        valid_events(*e),
        within(isha, DAY_BOUND as int),
    ensures
        r == maghrib_spec(*e, isha),
        within(r, DAY_BOUND as int),
{
    match (e.sunset, e.maghrib) {
        (None, _) => Slot::Missing(Missing::Sunset),
        (Some(set), MaghribBy::Sunset) => Slot::At(set),
        (Some(set), MaghribBy::Angle(None)) => Slot::Fallback(set, Rule::Sunset),
        (Some(set), MaghribBy::Angle(Some(t))) => {
            let clear = match isha {
                Slot::At(i) | Slot::Fallback(i, _) => i - t >= MIN_GAP,
                Slot::Missing(_) => true,
            };
            if t > set && clear {
                Slot::At(t)
            } else {
                Slot::Fallback(set, Rule::Sunset)
            }
        },
    }
}

/// A day's six times, Fajr to Isha.
pub type Six = [Slot; 6];

pub open spec fn ordered(s: Six) -> bool {
    &&& forall|i: int| 0 <= i < 6 ==> timed(#[trigger] s[i])
    &&& forall|i: int| 0 <= i < 5 ==> time_of(#[trigger] s[i + 1]) - time_of(s[i]) >= MIN_GAP
}

pub open spec fn all_within(s: Six, bound: int) -> bool {
    forall|i: int| 0 <= i < 6 ==> within(#[trigger] s[i], bound)
}

/// A day's times, resolved: all six in order, or not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resolved {
    /// All six, in order five minutes apart.
    Ordered(Six),
    /// Some missing (or all, when they were out of order).
    Partial(Six),
}

pub open spec fn rules_spec(e: Events, b: Bound, by: IshaBy) -> Six {
    let isha = isha_spec(e, b, by);
    [
        fajr_spec(e, b),
        match e.sunrise { Some(t) => Slot::At(t), None => Slot::Missing(Missing::Sunrise) },
        Slot::At(e.transit),
        match e.asr { Some(t) => Slot::At(t), None => Slot::Missing(Missing::Asr) },
        maghrib_spec(e, isha),
        isha,
    ]
}

/// Whether consecutive times are at least five minutes apart: exactly
/// `ordered`, for six timed slots.
fn in_order(s: &Six) -> (r: bool)
    requires
        forall|i: int| 0 <= i < 6 ==> timed(#[trigger] s[i]),
        all_within(*s, DAY_BOUND as int),
    ensures
        r == ordered(*s),
{
    let mut i: usize = 0;
    while i < 5
        invariant
            0 <= i <= 5,
            forall|i: int| 0 <= i < 6 ==> timed(#[trigger] s[i]),
            all_within(*s, DAY_BOUND as int),
            forall|j: int| 0 <= j < i ==> time_of(#[trigger] s[j + 1]) - time_of(s[j]) >= MIN_GAP,
        decreases 5 - i,
    {
        let a = match s[i] {
            Slot::At(t) | Slot::Fallback(t, _) => t,
            Slot::Missing(_) => 0,
        };
        let b = match s[i + 1] {
            Slot::At(t) | Slot::Fallback(t, _) => t,
            Slot::Missing(_) => 0,
        };
        assert(within(s[i as int], DAY_BOUND as int));
        assert(within(s[i as int + 1], DAY_BOUND as int));
        if b - a < MIN_GAP {
            return false;
        }
        i = i + 1;
    }
    true
}

/// The day's times from its events, by the rules (`rules_spec`): all six
/// in order exactly when they're all there five minutes apart; when all
/// are there but not in order, all missing (Missing::Order); else as the
/// rules leave them.
pub fn resolve(e: &Events, b: &Bound, by: IshaBy) -> (r: Resolved)
    requires
        valid_events(*e),
        valid_bound(*b),
        valid_isha_by(by),
    ensures
        match r {
            Resolved::Ordered(s) => s == rules_spec(*e, *b, by) && ordered(s) && all_within(
                s,
                DAY_BOUND as int,
            ),
            Resolved::Partial(s) => all_within(s, DAY_BOUND as int) && !ordered(rules_spec(*e, *b, by)) && (
            s == rules_spec(*e, *b, by) || forall|i: int| 0 <= i < 6 ==> #[trigger] s[i] == Slot::Missing(
                Missing::Order,
            )),
        },
{
    let isha = isha_of(e, b, by);
    let six: Six = [
        fajr_of(e, b),
        match e.sunrise {
            Some(t) => Slot::At(t),
            None => Slot::Missing(Missing::Sunrise),
        },
        Slot::At(e.transit),
        match e.asr {
            Some(t) => Slot::At(t),
            None => Slot::Missing(Missing::Asr),
        },
        maghrib_of(e, isha),
        isha,
    ];
    assert(six == rules_spec(*e, *b, by));
    let all_timed = match (six[0], six[1], six[2], six[3], six[4], six[5]) {
        (Slot::Missing(_), _, _, _, _, _) | (_, Slot::Missing(_), _, _, _, _) | (_, _, Slot::Missing(_), _, _, _)
        | (_, _, _, Slot::Missing(_), _, _) | (_, _, _, _, Slot::Missing(_), _) | (_, _, _, _, _, Slot::Missing(_)) => false,
        _ => true,
    };
    if !all_timed {
        assert(!timed(six[0]) || !timed(six[1]) || !timed(six[2]) || !timed(six[3]) || !timed(six[4]) || !timed(six[5]));
        return Resolved::Partial(six);
    }
    if in_order(&six) {
        Resolved::Ordered(six)
    } else {
        Resolved::Partial([Slot::Missing(Missing::Order); 6])
    }
}


// ------------------------------------------------------------ after the rules

pub open spec fn finished_slot(s: Slot, minutes: int, mode: Round) -> Slot {
    match s {
        Slot::At(t) => Slot::At(round_spec(t + 60 * minutes, mode) as i64),
        Slot::Fallback(t, r) => Slot::Fallback(round_spec(t + 60 * minutes, mode) as i64, r),
        Slot::Missing(m) => Slot::Missing(m),
    }
}

pub const FINISHED_BOUND: i64 = ADJUSTED_BOUND + 60;

fn finish_slot(s: Slot, minutes: i32, mode: Round) -> (r: Slot)
    requires
        within(s, DAY_BOUND as int),
        -MAX_OFFSET_MINUTES <= minutes <= MAX_OFFSET_MINUTES,
    ensures
        r == finished_slot(s, minutes as int, mode),
        within(r, FINISHED_BOUND as int),
{
    match s {
        Slot::At(t) => {
            let a = add_minutes(t, minutes);
            proof { lemma_round_bounds(a as int, mode); }
            Slot::At(round(a, mode))
        },
        Slot::Fallback(t, rule) => {
            let a = add_minutes(t, minutes);
            proof { lemma_round_bounds(a as int, mode); }
            Slot::Fallback(round(a, mode), rule)
        },
        Slot::Missing(m) => Slot::Missing(m),
    }
}

/// A day's times with their offsets (method's and user's, in minutes)
/// added and put on a minute: each exactly `finished_slot`.
pub fn finish(s: Six, minutes: [i32; 6], mode: Round) -> (r: Six)
    requires
        all_within(s, DAY_BOUND as int),
        forall|i: int| 0 <= i < 6 ==> -MAX_OFFSET_MINUTES <= #[trigger] minutes[i] <= MAX_OFFSET_MINUTES,
    ensures
        forall|i: int| 0 <= i < 6 ==> #[trigger] r[i] == finished_slot(s[i], minutes[i] as int, mode),
        all_within(r, FINISHED_BOUND as int),
{
    let r = [
        finish_slot(s[0], minutes[0], mode),
        finish_slot(s[1], minutes[1], mode),
        finish_slot(s[2], minutes[2], mode),
        finish_slot(s[3], minutes[3], mode),
        finish_slot(s[4], minutes[4], mode),
        finish_slot(s[5], minutes[5], mode),
    ];
    assert(forall|i: int| 0 <= i < 6 ==> #[trigger] r[i] == finished_slot(s[i], minutes[i] as int, mode));
    r
}

/// Without offsets, rounding keeps an ordered day in order: times five
/// minutes apart are still four apart.
pub proof fn lemma_finish_keeps_order(s: Six, mode: Round)
    requires
        ordered(s),
        all_within(s, DAY_BOUND as int),
    ensures
        forall|i: int| 0 <= i < 5 ==> time_of(#[trigger] finished_slot(s[i + 1], 0, mode)) - time_of(
            finished_slot(s[i], 0, mode),
        ) >= 240,
{
    assert forall|i: int| 0 <= i < 5 implies time_of(#[trigger] finished_slot(s[i + 1], 0, mode)) - time_of(
        finished_slot(s[i], 0, mode),
    ) >= 240 by {
        assert(timed(s[i]) && timed(s[i + 1]));
        assert(within(s[i], DAY_BOUND as int) && within(s[i + 1], DAY_BOUND as int));
        lemma_round_bounds(time_of(s[i]), mode);
        lemma_round_bounds(time_of(s[i + 1]), mode);
        assert(time_of(finished_slot(s[i], 0, mode)) == round_spec(time_of(s[i]), mode));
        assert(time_of(finished_slot(s[i + 1], 0, mode)) == round_spec(time_of(s[i + 1]), mode));
    }
}

// ------------------------------------------------------------ the night

pub open spec fn night_times_spec(maghrib: int, fajr: int, mode: Round) -> (int, int) {
    let night = fajr - maghrib;
    (round_spec(maghrib + night / 2, mode), round_spec(maghrib + (2 * night) / 3, mode))
}

/// The middle of the night from Maghrib to the next Fajr, and the start of
/// its last third, rounded (Adhan's SunnahTimes, floored to the second):
/// for a night that doesn't run backwards, between Maghrib and Fajr, in
/// order, when those are on a minute (as rounded times are).
pub fn night_times(maghrib: i64, fajr: i64, mode: Round) -> (r: (i64, i64))
    requires
        -MAX_SECONDS <= maghrib <= MAX_SECONDS,
        -MAX_SECONDS <= fajr <= MAX_SECONDS,
    ensures
        (r.0 as int, r.1 as int) == night_times_spec(maghrib as int, fajr as int, mode),
        maghrib <= fajr && (mode == Round::Exact || (maghrib % 60 == 0 && fajr % 60 == 0)) ==> maghrib
            <= r.0 <= r.1 <= fajr,
{
    let night = fajr - maghrib;
    let half = if night % 2 < 0 { night / 2 - 1 } else { night / 2 };
    let twice = 2 * night;
    let third = if twice % 3 < 0 { twice / 3 - 1 } else { twice / 3 };
    let (m, l) = (maghrib + half, maghrib + third);
    proof {
        let n = night as int;
        assert(half as int == n / 2);
        assert(third as int == (2 * n) / 3);
        if n >= 0 {
            assert(0 <= n / 2 <= (2 * n) / 3 <= n);
            lemma_round_monotone(maghrib as int, m as int, mode);
            lemma_round_monotone(m as int, l as int, mode);
            lemma_round_monotone(l as int, fajr as int, mode);
            if mode != Round::Exact && maghrib % 60 == 0 && fajr % 60 == 0 {
                lemma_round_minute(maghrib as int, mode);
                lemma_round_minute(fajr as int, mode);
            }
        }
    }
    (round(m, mode), round(l, mode))
}

// ------------------------------------------------------------ the moment's prayer

pub open spec fn latest_spec(times: Seq<i64>, now: i64, r: Option<usize>) -> bool {
    match r {
        Some(i) => i < times.len() && times[i as int] <= now && forall|j: int|
            0 <= j < times.len() && #[trigger] times[j] <= now ==> times[j] < times[i as int] || (
            times[j] == times[i as int] && j <= i),
        None => forall|j: int| 0 <= j < times.len() ==> #[trigger] times[j] > now,
    }
}

pub open spec fn earliest_spec(times: Seq<i64>, now: i64, r: Option<usize>) -> bool {
    match r {
        Some(i) => i < times.len() && times[i as int] > now && forall|j: int|
            0 <= j < times.len() && #[trigger] times[j] > now ==> times[j] > times[i as int] || (
            times[j] == times[i as int] && j >= i),
        None => forall|j: int| 0 <= j < times.len() ==> #[trigger] times[j] <= now,
    }
}

/// Of `times`, the latest at or before `now` (the last of equals): the
/// prayer whose time it is.
pub fn latest_at_or_before(times: &[i64], now: i64) -> (r: Option<usize>)
    ensures
        latest_spec(times@, now, r),
{
    let mut best: Option<usize> = None;
    let mut i: usize = 0;
    while i < times.len()
        invariant
            i <= times.len(),
            latest_spec(times@.subrange(0, i as int), now, best),
        decreases times.len() - i,
    {
        let t = times[i];
        if t <= now {
            match best {
                None => best = Some(i),
                Some(b) => if t >= times[b] {
                    best = Some(i);
                },
            }
        }
        assert(times@.subrange(0, i as int + 1) == times@.subrange(0, i as int).push(t));
        i = i + 1;
    }
    assert(times@.subrange(0, times.len() as int) == times@);
    best
}

/// Of `times`, the earliest after `now` (the first of equals): the next
/// prayer.
pub fn earliest_after(times: &[i64], now: i64) -> (r: Option<usize>)
    ensures
        earliest_spec(times@, now, r),
{
    let mut best: Option<usize> = None;
    let mut i: usize = 0;
    while i < times.len()
        invariant
            i <= times.len(),
            earliest_spec(times@.subrange(0, i as int), now, best),
        decreases times.len() - i,
    {
        let t = times[i];
        if t > now {
            match best {
                None => best = Some(i),
                Some(b) => if t < times[b] {
                    best = Some(i);
                },
            }
        }
        assert(times@.subrange(0, i as int + 1) == times@.subrange(0, i as int).push(t));
        i = i + 1;
    }
    assert(times@.subrange(0, times.len() as int) == times@);
    best
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding() {
        // 04:37:30 to the nearest minute is 04:38; salah 0.7's Up added a
        // minute to a time already on one.
        assert_eq!(round(16_650, Round::Nearest), 16_680);
        assert_eq!(round(16_649, Round::Nearest), 16_620);
        assert_eq!(round(-31, Round::Nearest), -60);
        assert_eq!(round(21_560, Round::Up), 21_600);
        assert_eq!(round(21_540, Round::Up), 21_540);
        assert_eq!(round(-59, Round::Up), 0);
        assert_eq!(round(21_560, Round::Exact), 21_560);
    }

    #[test]
    fn days_and_seconds_roll_over() {
        assert_eq!(day_and_second(86_400 + 3), (1, 3));
        assert_eq!(day_and_second(-1), (-1, 86_399));
        assert_eq!(absolute(1, -60), 86_340);
    }

    #[test]
    fn days_since_the_solstice() {
        // salah 0.7 overflowed south of the equator before late June.
        assert_eq!(days_since_solstice(15, 2026, false), 208);
        assert_eq!(days_since_solstice(258, 2026, false), 86);
        assert_eq!(days_since_solstice(360, 2026, true), 5);
        assert_eq!(days_since_solstice(1, 2024, false), 194);
    }

    #[test]
    fn the_prayer_of_a_moment() {
        let times = [10, 20, 20, 30];
        assert_eq!(latest_at_or_before(&times, 5), None);
        assert_eq!(latest_at_or_before(&times, 20), Some(2));
        assert_eq!(latest_at_or_before(&times, 99), Some(3));
        assert_eq!(earliest_after(&times, 10), Some(1));
        assert_eq!(earliest_after(&times, 30), None);
    }

    #[test]
    fn the_night() {
        // Maghrib 00:32, the next Fajr 08:43 (from 2015-07-13 00:00).
        assert_eq!(night_times(1_920, 31_380, Round::Nearest), (16_680, 21_540));
    }

    #[test]
    fn the_order_guard() {
        let at = |t| Slot::At(t);
        let ordered = [at(0), at(300), at(600), at(900), at(1200), at(1500)];
        assert!(in_order(&ordered));
        let crowded = [at(0), at(300), at(599), at(900), at(1200), at(1500)];
        assert!(!in_order(&crowded));
    }

    #[test]
    fn civil_dates() {
        assert!(Civil::new(2024, 2, 29).is_some());
        assert!(Civil::new(2023, 2, 29).is_none());
        assert!(Civil::new(3002, 1, 1).is_none());
        let last = Civil::new(MAX_YEAR, 12, 31).unwrap();
        assert_eq!(last.next(), None);
        let first = Civil::new(MIN_YEAR, 1, 1).unwrap();
        assert_eq!(first.previous(), None);
        let march = Civil::new(2024, 3, 1).unwrap();
        assert_eq!(march.previous(), Civil::new(2024, 2, 29));
        assert_eq!(march.previous().unwrap().number() + 1, march.number());
    }
}
