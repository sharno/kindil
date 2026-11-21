//! Every suspend, a nap's or a sleep's, as typestate: the wake alarm armed
//! as the very last thing before the suspend, read back from the RTC's
//! registers and checked ahead of it ([`crate::wakealarm::WakeRtc::arm`]),
//! or, for a sleep that deliberately has none, moved out of the way. The
//! caller has parked Wi-Fi. Each step is noted in the flight recorder
//! (naps.log) first; back awake, the alarm is moved out of the way and the
//! clock put back within the RTC's second.
//!
//! [`Suspend::suspend`] exists only on [`Armed`], reached by arming and
//! reading back, and on [`UntilButton`], reached only with an [`Asked`],
//! which only [`Why::Asked`] gives: a nap, or a sleep for the empty battery,
//! never suspends without an alarm that would wake it.
//!
//! And none suspends once the app has moved on: each nap or sleep carries a
//! [`Ticket`] from the app's [`Gate`], taken as the app sets it out; a
//! press (or a computer taking the storage) handled while its thread parks
//! Wi-Fi voids it, and a void ticket stops the suspend right before
//! `/sys/power/state` is written.
//!
//! ```compile_fail
//! // No suspend before the alarm is armed:
//! fn f(env: &kindil::suspend::Env) {
//!     let _ = kindil::suspend::Suspend::awake("nap").suspend(env);
//! }
//! ```
//!
//! ```compile_fail
//! // No sleep without an alarm but one the user asked for:
//! let _ = kindil::suspend::Asked(());
//! ```
//!
//! ```
//! use kindil::suspend::{Asked, Env, Suspend};
//! use kindil::system::Why;
//! fn sleep(env: &Env) {
//!     if let Some(asked) = Asked::of(Why::Asked) {
//!         let _ = Suspend::awake("sleep").until_button(env, asked).suspend(env);
//!     }
//! }
//! ```
use crate::clock::Correction;
use crate::system::Why;
use crate::wakealarm::Armed as Alarm;
use statum::{machine, state, transition};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The app's word on the naps and sleeps it set out: [`Gate::void`] once it
/// has left them (a press woke it, a computer took the storage).
#[derive(Clone, Debug, Default)]
pub struct Gate(Arc<AtomicU64>);

/// A nap's or sleep's leave to suspend, good until the app voids it.
#[derive(Clone, Debug)]
pub struct Ticket {
    gate: Gate,
    issued: u64,
}

impl Gate {
    /// For the nap or sleep the app sets out now.
    pub fn ticket(&self) -> Ticket {
        Ticket {
            gate: self.clone(),
            issued: self.0.load(Ordering::SeqCst),
        }
    }

    /// Every ticket taken so far is void.
    pub fn void(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl Ticket {
    /// Whether the app still wants this suspend.
    pub fn valid(&self) -> bool {
        self.gate.0.load(Ordering::SeqCst) == self.issued
    }
}

/// The RTC with the wake alarm ([`crate::wakealarm::WakeRtc`]).
pub trait Rtc {
    fn name(&self) -> String;
    /// Its time, in its own seconds.
    fn now(&self) -> anyhow::Result<i64>;
    /// Armed `after` from now, read back, and checked ahead of the RTC.
    fn arm(&self, after: Duration) -> anyhow::Result<Alarm>;
    /// Moved a year ahead.
    fn disarm(&self) -> anyhow::Result<()>;
}

impl Rtc for crate::wakealarm::WakeRtc {
    fn name(&self) -> String {
        self.name()
    }
    fn now(&self) -> anyhow::Result<i64> {
        self.now()
    }
    fn arm(&self, after: Duration) -> anyhow::Result<Alarm> {
        self.arm(after)
    }
    fn disarm(&self) -> anyhow::Result<()> {
        self.disarm()
    }
}

/// The rest of the device a suspend touches.
pub trait Kernel {
    /// A line for the flight recorder.
    fn note(&self, line: &str);
    /// `mem` to `/sys/power/state`: returns once resumed.
    fn mem(&self) -> std::io::Result<()>;
    /// Seconds since the epoch.
    fn epoch(&self) -> i64;
    /// The system clock caught up with the RTC, if it lost a second or more.
    fn catch_up(&self) -> Option<Correction>;
    /// Whether the kernel can take a suspend (src/health.rs).
    fn health(&self) -> crate::health::Health;
}

/// What a suspend needs: the RTC, if there is one, the kernel, and the
/// app's leave.
pub struct Env<'a> {
    pub rtc: Result<&'a dyn Rtc, &'a anyhow::Error>,
    pub kernel: &'a dyn Kernel,
    pub ticket: &'a Ticket,
}

impl Env<'_> {
    fn arm(&self, after: Duration) -> anyhow::Result<Alarm> {
        match self.rtc {
            Ok(rtc) => rtc.arm(after),
            Err(error) => Err(anyhow::anyhow!("{error}")),
        }
    }

    /// Moves the alarm out of the way, if there is one; a failure is noted.
    fn disarm(&self, what: &str) {
        if let Ok(rtc) = self.rtc {
            if let Err(error) = rtc.disarm() {
                self.kernel
                    .note(&format!("{what}: moving the alarm away: {error:#}"));
            }
        }
    }

    fn rtc_name(&self) -> String {
        self.rtc.map(|rtc| rtc.name()).unwrap_or_default()
    }
}

/// A sleep the user asked for (the power button, or the auto-sleep time):
/// the only one that may suspend without a wake alarm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Asked(());

impl Asked {
    pub fn of(why: Why) -> Option<Self> {
        match why {
            Why::Asked => Some(Asked(())),
            Why::BatteryEmpty => None,
        }
    }
}

/// How a suspend is to end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// By its alarm, this far ahead, or not at all: no alarm, no suspend.
    /// A nap's, and the empty battery's sleep's check.
    Alarm(Duration),
    /// By its alarm (a sleep's battery check), or, if none can be armed, by
    /// the power button alone.
    AlarmOrButton(Duration, Asked),
    /// By the power button alone: a sleep the user asked for, plugged in.
    Button(Asked),
}

impl Wake {
    /// The alarm it asks for.
    pub fn alarm(self) -> Option<Duration> {
        match self {
            Wake::Alarm(after) | Wake::AlarmOrButton(after, _) => Some(after),
            Wake::Button(_) => None,
        }
    }
}

/// What only this module can make.
pub struct Seal(());

#[state]
pub enum SuspendState {
    /// About to suspend; Wi-Fi parked.
    Awake,
    /// The alarm armed, read back and ahead of the RTC.
    Armed(Alarm),
    /// The alarm moved out of the way: only the power button wakes it.
    UntilButton,
    /// Back, with the clock caught up if it had to be.
    Resumed(Back),
}

#[machine]
pub struct Suspend<SuspendState> {
    /// `nap` or `sleep`, as naps.log names it.
    what: &'static str,
    seal: Seal,
}

impl Suspend<Awake> {
    pub fn awake(what: &'static str) -> Self {
        Self::builder().what(what).seal(Seal(())).build()
    }
}

#[transition]
impl Suspend<Awake> {
    /// The alarm, or no suspend at all.
    pub fn arm(
        self,
        env: &Env,
        after: Duration,
    ) -> ::core::result::Result<Suspend<Armed>, anyhow::Error> {
        match env.arm(after) {
            Ok(armed) => Ok(self.transition_with(armed)),
            Err(error) => {
                env.kernel
                    .note(&format!("{}: not suspending: {error:#}", self.what));
                Err(error.context("arming the wake alarm"))
            }
        }
    }

    /// The alarm, or, for a sleep the user asked for, the power button.
    pub fn arm_or_button(
        self,
        env: &Env,
        after: Duration,
        asked: Asked,
    ) -> ::statum::Branch<Suspend<Armed>, Suspend<UntilButton>> {
        match env.arm(after) {
            Ok(armed) => ::statum::Branch::First(self.transition_with(armed)),
            Err(error) => {
                crate::log!(
                    "kindil: {}: no wake alarm ({error:#}); until the power button",
                    self.what
                );
                ::statum::Branch::Second(self.until_button(env, asked))
            }
        }
    }

    /// No alarm: a sleep the user asked for.
    pub fn until_button(self, env: &Env, _: Asked) -> Suspend<UntilButton> {
        env.disarm(self.what);
        self.transition()
    }
}

#[transition]
impl Suspend<Armed> {
    pub fn suspend(self, env: &Env) -> Suspend<Resumed> {
        env.kernel.note(&format!(
            "{}: suspending, {} {}",
            self.what,
            env.rtc_name(),
            self.state_data
        ));
        let back = suspend(env, self.what, true);
        self.transition_with(back)
    }
}

#[transition]
impl Suspend<UntilButton> {
    pub fn suspend(self, env: &Env) -> Suspend<Resumed> {
        env.kernel
            .note(&format!("{}: suspending until the power button", self.what));
        let back = suspend(env, self.what, false);
        self.transition_with(back)
    }
}

/// Back from a suspend, or from the kernel's refusal of it.
#[derive(Debug)]
pub struct Back {
    /// Why the kernel wouldn't suspend, if it wouldn't.
    pub refused: Option<std::io::Error>,
    /// The clock's step on the way back, if it needed one.
    pub correction: Option<Correction>,
    /// Whether a wake alarm was armed for it: only then can it have woken it.
    pub armed: bool,
    /// The app voided its ticket first: it never suspended.
    pub cancelled: bool,
}

/// The suspend itself, and the way back.
fn suspend(env: &Env, what: &str, armed: bool) -> Back {
    // Checked as the very last thing: a press handled while Wi-Fi parked
    // woke the app, which mustn't be left on screen with the device asleep.
    // (The press can still come between this and the write: then the
    // kernel suspends, and that press, its wake source, ends it.)
    if !env.ticket.valid() {
        env.disarm(what);
        env.kernel
            .note(&format!("{what}: the app woke meanwhile; not suspending"));
        return Back {
            refused: None,
            correction: None,
            armed: false,
            cancelled: true,
        };
    }
    let since = env.kernel.epoch();
    let slept = env.kernel.mem();
    // The power button may have woken it first: the alarm must not wake a
    // later suspend that didn't ask for it.
    let rtc_now = env.rtc.ok().and_then(|rtc| rtc.now().ok());
    env.disarm(what);
    let correction = env.kernel.catch_up();
    env.kernel.note(&format!(
        "{what}: {} after {} s by the clock, {} at {}{}",
        match &slept {
            Ok(()) => "resumed".to_owned(),
            Err(error) => format!("suspend refused ({error})"),
        },
        env.kernel.epoch() - since,
        env.rtc_name(),
        rtc_now.map_or_else(|| "?".to_owned(), |n| n.to_string()),
        correction.map_or_else(String::new, |c| format!("; {c}")),
    ));
    Back {
        refused: slept.err(),
        correction,
        armed,
        cancelled: false,
    }
}

/// One suspend, ending as `wake` says: back (refused or not), or an error
/// if `wake` needs an alarm that couldn't be armed, with no suspend; or
/// [`crate::health::Unfit`], with no suspend and no alarm, once the kernel
/// has oopsed (every driver suspends with the device, the broken one too,
/// and on 2026-11-20 that one never returned) or lost too much memory.
pub fn once(env: &Env, what: &'static str, wake: Wake) -> anyhow::Result<Back> {
    match env.kernel.health() {
        crate::health::Health::Sound => {}
        crate::health::Health::Unfit(unfit) => {
            env.kernel.note(&format!("{what}: not suspending: {unfit}"));
            return Err(unfit.into());
        }
    }
    let awake = Suspend::awake(what);
    let resumed = match wake {
        Wake::Alarm(after) => awake.arm(env, after)?.suspend(env),
        Wake::AlarmOrButton(after, asked) => match awake.arm_or_button(env, after, asked) {
            ::statum::Branch::First(armed) => armed.suspend(env),
            ::statum::Branch::Second(until_button) => until_button.suspend(env),
        },
        Wake::Button(asked) => awake.until_button(env, asked).suspend(env),
    };
    Ok(resumed.state_data)
}

/// The machine's graph, for docs/state-machines.md.
#[cfg(test)]
pub(crate) fn graph() -> statum::StableGraphMetadata {
    use statum::MachineIntrospection;
    statum::StableGraphMetadata::from_graph(Suspend::<Awake>::GRAPH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// rtc1 at 1380, an alarm armed `after` ahead reading back right, unless
    /// `fails`; everything done to it logged.
    struct FakeRtc {
        fails: bool,
        log: RefCell<Vec<String>>,
    }

    impl Rtc for FakeRtc {
        fn name(&self) -> String {
            "rtc1".into()
        }
        fn now(&self) -> anyhow::Result<i64> {
            Ok(1_421)
        }
        fn arm(&self, after: Duration) -> anyhow::Result<Alarm> {
            self.log.borrow_mut().push("arm".into());
            anyhow::ensure!(!self.fails, "the alarm reads back 0, not the 1420 armed");
            let at = 1_380 + after.as_secs() as i64;
            Ok(Alarm {
                rtc: 1_380,
                at,
                read_back: Some(at),
            })
        }
        fn disarm(&self) -> anyhow::Result<()> {
            self.log.borrow_mut().push("disarm".into());
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeKernel {
        refuses: bool,
        unfit: Option<crate::health::Unfit>,
        notes: RefCell<Vec<String>>,
        suspends: RefCell<u32>,
    }

    impl Kernel for FakeKernel {
        fn note(&self, line: &str) {
            self.notes.borrow_mut().push(line.to_owned());
        }
        fn mem(&self) -> std::io::Result<()> {
            *self.suspends.borrow_mut() += 1;
            if self.refuses {
                return Err(std::io::Error::from_raw_os_error(libc::EBUSY));
            }
            Ok(())
        }
        fn epoch(&self) -> i64 {
            1_794_000_000
        }
        fn catch_up(&self) -> Option<Correction> {
            None
        }
        fn health(&self) -> crate::health::Health {
            self.unfit
                .map_or(crate::health::Health::Sound, crate::health::Health::Unfit)
        }
    }

    fn run(
        fails: bool,
        refuses: bool,
        what: &'static str,
        wake: Wake,
    ) -> (FakeRtc, FakeKernel, anyhow::Result<Back>) {
        run_with(fails, refuses, what, wake, &Gate::default().ticket())
    }

    fn run_with(
        fails: bool,
        refuses: bool,
        what: &'static str,
        wake: Wake,
        ticket: &Ticket,
    ) -> (FakeRtc, FakeKernel, anyhow::Result<Back>) {
        let rtc = FakeRtc {
            fails,
            log: RefCell::default(),
        };
        let kernel = FakeKernel {
            refuses,
            ..FakeKernel::default()
        };
        let back = once(
            &Env {
                rtc: Ok(&rtc),
                kernel: &kernel,
                ticket,
            },
            what,
            wake,
        );
        (rtc, kernel, back)
    }

    #[test]
    fn a_nap_arms_reads_back_suspends_and_moves_the_alarm_away() {
        let (rtc, kernel, back) = run(false, false, "nap", Wake::Alarm(Duration::from_secs(40)));
        let back = back.unwrap();
        assert!(back.refused.is_none() && back.armed);
        assert_eq!(*rtc.log.borrow(), ["arm", "disarm"]);
        assert_eq!(
            *kernel.notes.borrow(),
            [
                "nap: suspending, rtc1 alarm at 1420 (rtc 1380 +40 s, read back 1420)",
                "nap: resumed after 0 s by the clock, rtc1 at 1421",
            ]
        );
    }

    #[test]
    fn no_alarm_no_nap_and_no_battery_sleep() {
        for what in ["nap", "sleep"] {
            let (rtc, kernel, back) = run(true, false, what, Wake::Alarm(Duration::from_secs(40)));
            let error = back.unwrap_err();
            assert!(
                format!("{error:#}").contains("arming the wake alarm"),
                "{error:#}"
            );
            assert_eq!(*kernel.suspends.borrow(), 0, "{what}");
            assert_eq!(*rtc.log.borrow(), ["arm"]);
            assert_eq!(
                *kernel.notes.borrow(),
                [format!(
                    "{what}: not suspending: the alarm reads back 0, not the 1420 armed"
                )]
            );
        }
    }

    #[test]
    fn a_sleep_the_user_asked_for_falls_back_to_the_power_button() {
        let asked = Asked::of(Why::Asked).unwrap();
        let (rtc, kernel, back) = run(
            true,
            false,
            "sleep",
            Wake::AlarmOrButton(Duration::from_secs(40), asked),
        );
        assert!(!back.unwrap().armed, "no alarm: only the button wakes it");
        assert_eq!(*rtc.log.borrow(), ["arm", "disarm", "disarm"]);
        assert_eq!(
            kernel.notes.borrow()[0],
            "sleep: suspending until the power button"
        );
        let (rtc, kernel, _) = run(false, false, "sleep", Wake::Button(asked));
        assert_eq!(*rtc.log.borrow(), ["disarm", "disarm"], "never armed");
        assert_eq!(*kernel.suspends.borrow(), 1);
    }

    /// verify/SleepRaces.cfg's counterexample: the app handled a press while
    /// the nap's thread parked Wi-Fi; the nap mustn't suspend behind it.
    #[test]
    fn a_ticket_voided_while_wifi_parks_stops_the_suspend() {
        let gate = Gate::default();
        let ticket = gate.ticket();
        gate.void();
        for wake in [
            Wake::Alarm(Duration::from_secs(40)),
            Wake::Button(Asked::of(Why::Asked).unwrap()),
        ] {
            let (rtc, kernel, back) = run_with(false, false, "nap", wake, &ticket);
            assert!(back.unwrap().cancelled);
            assert_eq!(*kernel.suspends.borrow(), 0, "{wake:?}");
            assert_eq!(rtc.log.borrow().last().map(String::as_str), Some("disarm"));
            assert_eq!(
                kernel.notes.borrow().last().map(String::as_str),
                Some("nap: the app woke meanwhile; not suspending")
            );
        }
        // A ticket taken after the void is good.
        let (_, kernel, _) = run_with(
            false,
            false,
            "nap",
            Wake::Alarm(Duration::from_secs(40)),
            &gate.ticket(),
        );
        assert_eq!(*kernel.suspends.borrow(), 1);
    }

    #[test]
    fn a_refused_suspend_still_comes_back_and_says_so() {
        let (rtc, kernel, back) = run(false, true, "nap", Wake::Alarm(Duration::from_secs(40)));
        assert!(back.unwrap().refused.is_some());
        assert_eq!(*rtc.log.borrow(), ["arm", "disarm"]);
        assert!(kernel.notes.borrow()[1].starts_with("nap: suspend refused ("));
    }

    #[test]
    fn without_an_rtc_only_the_power_button_wakes() {
        let kernel = FakeKernel::default();
        let missing = anyhow::anyhow!("no RTC with a wake alarm");
        let ticket = Gate::default().ticket();
        let env = Env {
            rtc: Err(&missing),
            kernel: &kernel,
            ticket: &ticket,
        };
        assert!(once(&env, "nap", Wake::Alarm(Duration::from_secs(40))).is_err());
        assert_eq!(
            *kernel.notes.borrow(),
            ["nap: not suspending: no RTC with a wake alarm"]
        );
        let asked = Asked::of(Why::Asked).unwrap();
        once(&env, "sleep", Wake::Button(asked)).unwrap();
        assert_eq!(
            kernel.notes.borrow()[1..],
            [
                "sleep: suspending until the power button",
                "sleep: resumed after 0 s by the clock,  at ?"
            ]
        );
    }

    /// 2026-11-20: the Wi-Fi driver oopsed out of memory, and the next nap's
    /// suspend never came back. After an oops nothing suspends, a sleep the
    /// user asked for neither, and no alarm is armed.
    #[test]
    fn nothing_suspends_after_an_oops() {
        let asked = Asked::of(Why::Asked).unwrap();
        for (what, wake) in [
            ("nap", Wake::Alarm(Duration::from_secs(40))),
            ("sleep", Wake::AlarmOrButton(Duration::from_secs(40), asked)),
            ("sleep", Wake::Button(asked)),
        ] {
            let rtc = FakeRtc {
                fails: false,
                log: RefCell::default(),
            };
            let kernel = FakeKernel {
                unfit: Some(crate::health::Unfit::Oopsed),
                ..FakeKernel::default()
            };
            let ticket = Gate::default().ticket();
            let env = Env {
                rtc: Ok(&rtc),
                kernel: &kernel,
                ticket: &ticket,
            };
            let error = once(&env, what, wake).unwrap_err();
            assert_eq!(
                error.downcast_ref::<crate::health::Unfit>(),
                Some(&crate::health::Unfit::Oopsed),
                "{error:#}"
            );
            assert_eq!(*kernel.suspends.borrow(), 0, "{wake:?}");
            assert!(rtc.log.borrow().is_empty(), "{wake:?}: no alarm touched");
            assert_eq!(
                *kernel.notes.borrow(),
                [format!(
                    "{what}: not suspending: the kernel has oopsed since boot (TAINT_DIE); \
                     a suspend could hang"
                )]
            );
        }
    }
}
