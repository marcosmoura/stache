//! `menu-native-check`: the opt-in manual gate harness for `MenuAnywhere` Part 02.
//!
//! It drives the real Accessibility reader, `NSMenu` presenter, tracking-mode
//! dispatch and exact-instance activation/`AXPress` against **one disposable
//! test application that the tester launched**. It is never built or run by
//! default (`required-features = ["menu-native-check"]`) and the automated test
//! suite never invokes it.
//!
//! ```text
//! menu-native-check <read-show|cancel-during-tracking|stall-measure|select-noop>
//!     --pid <PID> --bundle-id <ID> --x <QUARTZ_X> --y <QUARTZ_Y>
//!     --i-understand-this-drives-a-real-app
//! ```
//!
//! Safety: it refuses to run without every argument, never terminates, signals
//! or launches any process (suspend/resume of the test PID and quitting it stay
//! with the tester), never kills by executable name and never touches legacy
//! Stache. `--bundle-id` must name exactly one running instance. The adapter
//! does not expose a PID to instance mapping, so the tester states both and the
//! harness only reads the explicit `--pid` while activating the sole running
//! instance of `--bundle-id`.

#![forbid(unsafe_code)]

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use stache_macos::menu::{
    AxOwner, CancelOutcome, CleanupOutcome, ControlConfig, MenuControl, MenuEvent,
    MenuPresentationOutcome, MenuReadOutcome, MenuTarget, OwnerConfig, PresentationAuthority,
    PresentationId, QuartzPoint, ReadRequest, RunLoopDispatcher, ShowRequest, SystemGeometry,
    SystemMenuClock, SystemPresenter, TargetCheck, TargetValidator, run_main_loop,
};
use stache_macos::{ApplicationControl, ApplicationObservation, MainThread};
use stache_sdk::CancellationToken;

const CONFIRM_FLAG: &str = "--i-understand-this-drives-a-real-app";
const USAGE: &str = "usage: menu-native-check <read-show|cancel-during-tracking|stall-measure|\
select-noop> --pid <PID> --bundle-id <ID> --x <QUARTZ_X> --y <QUARTZ_Y> \
--i-understand-this-drives-a-real-app";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Gate {
    ReadShow,
    CancelDuringTracking,
    StallMeasure,
    SelectNoop,
}

#[derive(Clone)]
struct Args {
    gate: Gate,
    pid: i32,
    bundle_id: String,
    anchor: QuartzPoint,
}

fn parse(arguments: &[String]) -> Result<Args, String> {
    let mut iterator = arguments.iter();
    let gate = match iterator.next().map(String::as_str) {
        Some("read-show") => Gate::ReadShow,
        Some("cancel-during-tracking") => Gate::CancelDuringTracking,
        Some("stall-measure") => Gate::StallMeasure,
        Some("select-noop") => Gate::SelectNoop,
        _ => return Err("a gate subcommand is required".to_owned()),
    };
    let (mut pid, mut bundle, mut x, mut y, mut confirmed) = (None, None, None, None, false);
    while let Some(flag) = iterator.next() {
        let mut value =
            |name: &str| iterator.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match flag.as_str() {
            "--pid" => pid = Some(value("--pid")?.parse::<i32>().map_err(|e| e.to_string())?),
            "--bundle-id" => bundle = Some(value("--bundle-id")?),
            "--x" => x = Some(value("--x")?.parse::<f64>().map_err(|e| e.to_string())?),
            "--y" => y = Some(value("--y")?.parse::<f64>().map_err(|e| e.to_string())?),
            CONFIRM_FLAG => confirmed = true,
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    if !confirmed {
        return Err(format!("{CONFIRM_FLAG} is required"));
    }
    let pid = pid.filter(|pid| *pid > 1).ok_or("--pid <PID> is required")?;
    Ok(Args {
        gate,
        pid,
        bundle_id: bundle.ok_or("--bundle-id is required")?,
        anchor: QuartzPoint {
            x: x.ok_or("--x is required")?,
            y: y.ok_or("--y is required")?,
        },
    })
}

struct LiveValidator(Arc<ApplicationObservation>);

impl TargetValidator for LiveValidator {
    fn validate(&self, target: &MenuTarget) -> TargetCheck {
        match self.0.is_running(target.application) {
            Ok(true) => TargetCheck::Current,
            Ok(false) => TargetCheck::Stale,
            Err(_) => TargetCheck::Unavailable,
        }
    }
}

struct Always;

impl PresentationAuthority for Always {
    fn is_current(&self) -> bool { true }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse(&arguments) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("menu-native-check: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run_main_loop(move || run(&args)) {
        Some(Ok(())) => ExitCode::SUCCESS,
        Some(Err(message)) => {
            eprintln!("menu-native-check: {message}");
            ExitCode::FAILURE
        }
        None => {
            eprintln!("menu-native-check: must run on the main thread");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<(), String> {
    let started = Instant::now();
    let applications = ApplicationControl::system(Arc::new(MainThread));
    let observation = Arc::new(
        applications
            .install(&CancellationToken::new(), |_| {})
            .map_err(|e| format!("{e:?}"))?,
    );
    let running = observation
        .running_applications(&args.bundle_id)
        .map_err(|e| format!("{e:?}"))?;
    let [application] = running.as_slice() else {
        return Err(format!(
            "{} instances of {} are running; exactly one is required",
            running.len(),
            args.bundle_id
        ));
    };
    let accepted = observation.accept().map_err(|e| format!("{e:?}"))?;
    let target = MenuTarget {
        pid: args.pid,
        application: *application,
    };
    let owner = Arc::new(AxOwner::system(OwnerConfig {
        clock: Arc::new(SystemMenuClock),
        validator: Arc::new(LiveValidator(Arc::clone(&observation))),
        own_pid: i32::try_from(std::process::id()).unwrap_or(0),
    }));
    let control = MenuControl::new(
        Arc::clone(&owner),
        Arc::new(SystemPresenter),
        Arc::new(SystemGeometry),
        Arc::new(RunLoopDispatcher),
        Arc::new(SystemMenuClock),
        ControlConfig::default(),
    );
    let events = control.take_events().ok_or("events already taken")?;

    // Read.
    let presentation = PresentationId::allocate();
    let (tx, rx) = std::sync::mpsc::channel();
    let read_started = Instant::now();
    owner
        .submit_read(
            ReadRequest {
                target: Some(target),
                presentation,
                cancel: CancellationToken::new(),
                deadline: None,
            },
            move |outcome| {
                let _ = tx.send(outcome);
            },
        )
        .map_err(|e| format!("read not queued: {e:?}"))?;
    let outcome = rx.recv_timeout(Duration::from_secs(10)).map_err(|e| e.to_string())?;
    println!("read: {:?} after {:?}", short(&outcome), read_started.elapsed());
    let MenuReadOutcome::Snapshot(snapshot) = outcome else {
        return Ok(());
    };
    if args.gate == Gate::StallMeasure {
        println!("stall-measure: read finished; suspend/resume of the PID is the tester's step");
        println!("cleanup: {:?}", control.release(presentation));
        return Ok(());
    }

    // Show.
    let shown = control.show(ShowRequest {
        snapshot,
        anchor: args.anchor,
        expected_geometry: None,
        target,
        accepted,
        authority: Arc::new(Always),
        deadline: Instant::now() + Duration::from_secs(5),
    });
    println!("show: {shown:?} at +{:?}", started.elapsed());
    if !matches!(shown, MenuPresentationOutcome::Shown { .. }) {
        return Ok(());
    }

    match args.gate {
        Gate::CancelDuringTracking => {
            std::thread::sleep(Duration::from_millis(1500));
            let begun = Instant::now();
            let cancel = control.cancel(presentation);
            println!("cancel: {cancel:?} after {:?}", begun.elapsed());
            println!(
                "verdict: {}",
                if cancel == CancelOutcome::Acknowledged {
                    "cancelTracking ran DURING tracking"
                } else {
                    "cancellation did NOT run during tracking: gate FAILED"
                }
            );
            drain(&events, Duration::from_secs(5));
        }
        Gate::ReadShow => drain(&events, Duration::from_secs(120)),
        Gate::SelectNoop => select_noop(&control, &observation, &events),
        Gate::StallMeasure => {}
    }
    print_cleanup(&control.release(presentation));
    Ok(())
}

fn select_noop(
    control: &MenuControl,
    observation: &ApplicationObservation,
    events: &std::sync::mpsc::Receiver<MenuEvent>,
) {
    // A selection is delivered before its menu's close; after a close without
    // one, wait only briefly instead of the full selection window.
    let mut wait = Duration::from_secs(120);
    loop {
        match events.recv_timeout(wait) {
            Ok(MenuEvent::Selected(selection)) => {
                let begun = Instant::now();
                let report = control.invoke(
                    &selection,
                    observation,
                    Instant::now() + Duration::from_secs(10),
                );
                println!("invoke: {report:?} after {:?}", begun.elapsed());
                return;
            }
            Ok(MenuEvent::Closed(id)) => {
                println!("closed: {id:?}");
                wait = Duration::from_secs(1);
            }
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                println!("no selection");
                return;
            }
        }
    }
}

fn drain(events: &std::sync::mpsc::Receiver<MenuEvent>, wait: Duration) {
    let deadline = Instant::now() + wait;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match events.recv_timeout(remaining) {
            Ok(MenuEvent::Selected(selection)) => println!("selected: {:?}", selection.token()),
            Ok(MenuEvent::Closed(id)) => {
                println!("closed: {id:?}");
                return;
            }
            Err(_) => return,
        }
    }
}

fn print_cleanup(outcome: &CleanupOutcome) {
    println!("cleanup: {outcome:?}");
}

fn short(outcome: &MenuReadOutcome) -> String {
    match outcome {
        MenuReadOutcome::Snapshot(snapshot) => {
            format!("Snapshot({} root items)", snapshot.items.len())
        }
        other => format!("{other:?}"),
    }
}
