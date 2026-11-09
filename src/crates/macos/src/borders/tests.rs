use std::cell::RefCell;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use stache_sdk::commands::{
    ChildProcess, CommandEnvironment, CommandExecutor, ProcessFactory, SpawnSpec, SystemClock,
};
use stache_sdk::threading::CancellationToken;

use super::mach::{Connection, MachApi, MachMessage};
use super::testing::FakeTransport;
use super::*;

fn strings(args: &[&str]) -> Vec<String> { args.iter().map(|a| (*a).to_owned()).collect() }

// ---- layout (compile-time asserts live in mach.rs; these pin field values) ----

#[test]
fn message_fields_match_the_legacy_wire_image() {
    let payload = [b'a', 0, 0];
    let message = MachMessage::new(77, &payload);
    assert_eq!(message.bits(), 0x8000_0013);
    assert_eq!(message.message_size(), 44);
    assert_eq!(message.remote_port(), 77);
    assert_eq!(message.descriptor_count(), 1);
    assert_eq!(message.payload_len(), 3);
    assert_eq!(message.deallocate_flag(), 0);
    assert_eq!(message.copy_option(), 1);
    assert_eq!(message.descriptor_type(), 1);
    assert_eq!(message.payload_ptr(), payload.as_ptr());
}

// ---- encoder ----

#[test]
fn encoder_matches_legacy_vectors() {
    assert_eq!(encode_mach_args(&[]).unwrap(), vec![0]);
    assert_eq!(encode_mach_args(&strings(&["a"])).unwrap(), b"a\0\0");
    assert_eq!(
        encode_mach_args(&strings(&["width=4.0", "style=round"])).unwrap(),
        b"width=4.0\0style=round\0\0"
    );
}

#[test]
fn encoder_distinguishes_argument_boundaries() {
    let left = encode_mach_args(&strings(&["a b", "c"])).unwrap();
    let right = encode_mach_args(&strings(&["a", "b c"])).unwrap();
    let concatenated = encode_mach_args(&strings(&["ab"])).unwrap();
    assert_ne!(left, right);
    assert_ne!(encode_mach_args(&strings(&["a", "b"])).unwrap(), concatenated);
}

#[test]
fn encoder_rejects_nul_empty_and_oversized_arguments() {
    assert_eq!(
        encode_mach_args(&strings(&["ok", "a\0b"])),
        Err(EncodeError::InteriorNul { index: 1 })
    );
    assert_eq!(
        encode_mach_args(&strings(&["ok", ""])),
        Err(EncodeError::EmptyArgument { index: 1 })
    );
    assert_eq!(
        encode_mach_args(&["x".repeat(MAX_PAYLOAD_BYTES)]),
        Err(EncodeError::TooLarge)
    );
}

#[test]
fn encoder_passes_shell_metacharacters_verbatim() {
    let args = strings(&["a;b", "$(reboot)", "\"q\"", "`x`", "a|b", "~"]);
    let bytes = encode_mach_args(&args).unwrap();
    assert_eq!(bytes, b"a;b\0$(reboot)\0\"q\"\0`x`\0a|b\0~\0\0");
}

// ---- timeout and classification ----

#[test]
fn timeout_is_clamped_to_one_through_five_hundred_ms() {
    assert_eq!(send_timeout_ms(Duration::ZERO), 1);
    assert_eq!(send_timeout_ms(Duration::from_micros(10)), 1);
    assert_eq!(send_timeout_ms(Duration::from_millis(100)), 100);
    assert_eq!(send_timeout_ms(Duration::from_millis(500)), 500);
    assert_eq!(send_timeout_ms(Duration::from_secs(60)), 500);
}

#[test]
fn kern_return_mapping() {
    assert_eq!(classify_send_result(0), SendOutcome::Sent);
    for code in [0x1000_0003, 0x1000_000a] {
        assert_eq!(
            classify_send_result(code),
            SendOutcome::MissingService,
            "a dead destination is a missing service, not a validation rejection"
        );
    }
    for code in [0x1000_0002, 0x1000_000c, 0x1000_0017, 0x1000_000e] {
        assert_eq!(
            classify_send_result(code),
            SendOutcome::Rejected(Rejection::Kernel(code))
        );
    }
    for code in [
        super::mach::MACH_SEND_TIMED_OUT,
        0x1000_0007,
        0x1000_0001,
        5,
        -1,
    ] {
        assert_eq!(classify_send_result(code), SendOutcome::Uncertain(code));
    }
}

// ---- connection over a fake kernel ----

#[derive(Default)]
struct KernelState {
    lookup: Option<Result<u32, i32>>,
    send_code: i32,
    deallocated: Vec<u32>,
    sent: Vec<(u32, Vec<u8>, u32, u32, u32)>,
}

#[derive(Clone, Default)]
struct FakeMach(Rc<RefCell<KernelState>>);

impl MachApi for FakeMach {
    fn look_up(&self, service: &str) -> Result<u32, i32> {
        assert_eq!(service, "git.felix.borders");
        self.0.borrow().lookup.unwrap_or(Err(1102))
    }

    fn send(&self, message: &mut MachMessage, timeout_ms: u32) -> i32 {
        let len = message.payload_len() as usize;
        // SAFETY: the connection keeps the payload alive for the whole call and
        // `len` is the descriptor's declared size.
        let bytes = unsafe { std::slice::from_raw_parts(message.payload_ptr(), len) }.to_vec();
        let mut state = self.0.borrow_mut();
        state.sent.push((
            message.remote_port(),
            bytes,
            timeout_ms,
            message.descriptor_count(),
            message.message_size(),
        ));
        state.send_code
    }

    fn deallocate(&self, port: u32) { self.0.borrow_mut().deallocated.push(port); }
}

fn fake_connection(lookup: Result<u32, i32>) -> (Connection<FakeMach>, FakeMach) {
    let kernel = FakeMach::default();
    kernel.0.borrow_mut().lookup = Some(lookup);
    (Connection::new(kernel.clone()), kernel)
}

#[test]
fn missing_service_sends_nothing() {
    let (mut connection, kernel) = fake_connection(Err(1102));
    assert_eq!(connection.lookup(), Err(MissingService { code: 1102 }));
    assert_eq!(
        connection.send(&strings(&["width=1"]), Duration::from_millis(50)),
        SendOutcome::MissingService
    );
    assert!(kernel.0.borrow().sent.is_empty());
    assert!(kernel.0.borrow().deallocated.is_empty());
}

#[test]
fn successful_send_carries_encoded_argv_and_bounded_timeout() {
    let (mut connection, kernel) = fake_connection(Ok(9));
    connection.lookup().unwrap();
    let outcome = connection.send(
        &strings(&["active_color=0xff", "width=4"]),
        Duration::from_secs(9),
    );
    assert_eq!(outcome, SendOutcome::Sent);
    let state = kernel.0.borrow();
    assert_eq!(state.sent.len(), 1);
    let (port, bytes, timeout, descriptors, size) = &state.sent[0];
    assert_eq!((*port, *timeout, *descriptors, *size), (9, 500, 1, 44));
    assert_eq!(bytes.as_slice(), b"active_color=0xff\0width=4\0\0");
}

#[test]
fn invalid_arguments_never_reach_the_kernel() {
    let (mut connection, kernel) = fake_connection(Ok(9));
    connection.lookup().unwrap();
    let outcome = connection.send(&strings(&["a\0b"]), Duration::from_millis(10));
    assert_eq!(
        outcome,
        SendOutcome::Rejected(Rejection::InvalidArguments(EncodeError::InteriorNul {
            index: 0
        }))
    );
    assert!(kernel.0.borrow().sent.is_empty());
}

#[test]
fn rejection_and_uncertain_are_distinct_and_a_dead_destination_is_missing() {
    let (mut connection, kernel) = fake_connection(Ok(9));
    connection.lookup().unwrap();
    kernel.0.borrow_mut().send_code = 0x1000_0004;
    assert_eq!(
        connection.send(&strings(&["x=1"]), Duration::from_millis(10)),
        SendOutcome::Uncertain(0x1000_0004)
    );
    assert!(
        kernel.0.borrow().deallocated.is_empty(),
        "a timeout keeps the right"
    );
    kernel.0.borrow_mut().send_code = 0x1000_0003;
    assert_eq!(
        connection.send(&strings(&["x=1"]), Duration::from_millis(10)),
        SendOutcome::MissingService
    );
    assert_eq!(
        kernel.0.borrow().deallocated,
        vec![9],
        "a dead destination drops the right"
    );
    assert_eq!(
        connection.send(&strings(&["x=1"]), Duration::from_millis(10)),
        SendOutcome::MissingService
    );
    drop(connection);
    assert_eq!(kernel.0.borrow().deallocated, vec![9], "released exactly once");
}

#[test]
fn right_is_deallocated_exactly_once_on_release_and_drop() {
    let (mut connection, kernel) = fake_connection(Ok(5));
    connection.lookup().unwrap();
    connection.release();
    connection.release();
    drop(connection);
    assert_eq!(kernel.0.borrow().deallocated, vec![5]);
}

#[test]
fn drop_without_release_deallocates_once_and_sends_nothing() {
    let (mut connection, kernel) = fake_connection(Ok(5));
    connection.lookup().unwrap();
    drop(connection);
    assert_eq!(kernel.0.borrow().deallocated, vec![5]);
    assert!(kernel.0.borrow().sent.is_empty());
}

#[test]
fn replacing_lookup_releases_the_previous_right() {
    let (mut connection, kernel) = fake_connection(Ok(5));
    connection.lookup().unwrap();
    kernel.0.borrow_mut().lookup = Some(Ok(6));
    connection.lookup().unwrap();
    assert_eq!(kernel.0.borrow().deallocated, vec![5]);
    kernel.0.borrow_mut().lookup = Some(Err(1102));
    assert!(connection.lookup().is_err());
    assert_eq!(kernel.0.borrow().deallocated, vec![5, 6]);
    drop(connection);
    assert_eq!(kernel.0.borrow().deallocated, vec![5, 6]);
}

#[test]
fn real_connection_is_unconnected_and_inert_until_lookup() {
    // No lookup and no send: constructing and dropping touches no service.
    let mut connection = BordersConnection::new();
    connection.release();
    assert_eq!(
        BordersTransport::send(&mut connection, &strings(&["x=1"]), Duration::from_millis(1)),
        SendOutcome::MissingService
    );
}

// ---- attempt gating ----

#[test]
fn missing_lookup_yields_no_attempt_and_no_send() {
    let mut fake = FakeTransport::missing();
    let observer = fake.clone();
    assert!(ServiceAttempt::begin(&mut fake).is_err());
    assert_eq!(observer.lookups(), 1);
    assert!(observer.sends().is_empty());
}

#[test]
fn every_attempt_looks_up_first() {
    let mut fake = FakeTransport::present();
    let observer = fake.clone();
    for _ in 0..2 {
        let mut attempt = ServiceAttempt::begin(&mut fake).unwrap();
        assert_eq!(
            attempt.send(&strings(&["a=1"]), Duration::from_millis(10)),
            SendOutcome::Sent
        );
    }
    assert_eq!(observer.lookups(), 2);
    assert_eq!(observer.sends().len(), 2);
}

// ---- CLI helper over a fake process factory ----

#[derive(Default)]
struct FakeFactory {
    specs: Mutex<Vec<SpawnSpec>>,
    exit_code: i32,
}

struct FakeChild(i32);

impl ChildProcess for FakeChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Ok(Some(ExitStatus::from_raw(self.0 << 8)))
    }

    fn kill(&mut self) -> io::Result<()> { Ok(()) }

    fn read_stdout(&mut self, _: &mut [u8]) -> io::Result<usize> { Ok(0) }

    fn read_stderr(&mut self, _: &mut [u8]) -> io::Result<usize> { Ok(0) }
}

impl ProcessFactory for FakeFactory {
    fn spawn(&self, spec: &SpawnSpec) -> io::Result<Box<dyn ChildProcess>> {
        self.specs.lock().unwrap().push(spec.clone());
        Ok(Box::new(FakeChild(self.exit_code)))
    }
}

fn runner(exit_code: i32) -> (CliRunner, Arc<FakeFactory>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("borders");
    std::fs::write(&binary, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let environment = CommandEnvironment {
        extra_paths: Some(dir.path().as_os_str().to_owned()),
        ..CommandEnvironment::default()
    };
    let factory = Arc::new(FakeFactory {
        specs: Mutex::default(),
        exit_code,
    });
    let executor =
        CommandExecutor::with_parts(factory.clone(), Arc::new(SystemClock::default()), environment);
    (CliRunner::new(executor), factory, dir)
}

#[test]
fn cli_fallback_runs_after_a_successful_lookup_with_exact_argv() {
    let (runner, factory, dir) = runner(0);
    let mut fake = FakeTransport::present();
    let attempt = ServiceAttempt::begin(&mut fake).unwrap();
    let cancel = CancellationToken::new();
    let args = strings(&["a b", "$(x);y", "\"q\""]);
    assert_eq!(
        attempt.cli_fallback(&runner, &args, &cancel, &cancel),
        CliOutcome::Succeeded
    );
    let specs = factory.specs.lock().unwrap();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].program, dir.path().join("borders"));
    assert_eq!(specs[0].arguments, args);
}

#[test]
fn cli_not_invoked_when_lookup_fails() {
    let (_runner, factory, _dir) = runner(0);
    let mut fake = FakeTransport::missing();
    assert!(ServiceAttempt::begin(&mut fake).is_err());
    assert!(factory.specs.lock().unwrap().is_empty());
}

#[test]
fn cli_rejects_nul_and_empty_arguments_before_spawning() {
    let (runner, factory, _dir) = runner(0);
    let mut fake = FakeTransport::present();
    let attempt = ServiceAttempt::begin(&mut fake).unwrap();
    let cancel = CancellationToken::new();
    assert_eq!(
        attempt.cli_fallback(&runner, &strings(&["a\0b"]), &cancel, &cancel),
        CliOutcome::InvalidArguments(EncodeError::InteriorNul { index: 0 })
    );
    assert_eq!(
        attempt.cli_fallback(&runner, &strings(&[""]), &cancel, &cancel),
        CliOutcome::InvalidArguments(EncodeError::EmptyArgument { index: 0 })
    );
    assert!(factory.specs.lock().unwrap().is_empty());
}

#[test]
fn cli_reports_failure_exit_and_cancellation() {
    let (runner, factory, _dir) = runner(3);
    let mut fake = FakeTransport::present();
    let attempt = ServiceAttempt::begin(&mut fake).unwrap();
    let cancel = CancellationToken::new();
    assert_eq!(
        attempt.cli_fallback(&runner, &strings(&["a=1"]), &cancel, &cancel),
        CliOutcome::ExitedWithFailure(Some(3))
    );
    cancel.cancel();
    assert_eq!(
        attempt.cli_fallback(&runner, &strings(&["a=1"]), &cancel, &cancel),
        CliOutcome::Cancelled
    );
    assert_eq!(factory.specs.lock().unwrap().len(), 1);
}

#[test]
fn cli_bounds_are_the_documented_ones() {
    assert_eq!(CLI_CHILD_DEADLINE, Duration::from_secs(2));
    assert_eq!(CLI_OUTPUT_CAP, 64 * 1024);
    assert_eq!(MACH_SEND_DEADLINE_MAX, Duration::from_millis(500));
    assert_eq!(SERVICE_NAME, "git.felix.borders");
}
