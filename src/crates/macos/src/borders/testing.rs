//! Fakes for the borders transport. Never touch bootstrap, Mach or processes.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use super::transport::{BordersTransport, MissingService, SendOutcome};

/// One recorded send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeSend {
    /// The argv given to the transport.
    pub args: Vec<String>,
    /// The deadline given to the transport.
    pub deadline: Duration,
}

#[derive(Debug, Default)]
struct FakeState {
    service_present: bool,
    lookups: usize,
    sends: Vec<FakeSend>,
    scripted: VecDeque<SendOutcome>,
    vanish_on_send: bool,
}

/// A scripted [`BordersTransport`]. Clones share state so a test can observe
/// what a runner did.
#[derive(Clone, Debug, Default)]
pub struct FakeTransport {
    state: Rc<RefCell<FakeState>>,
}

impl FakeTransport {
    /// A fake whose lookup succeeds and whose sends return `Sent`.
    #[must_use]
    pub fn present() -> Self {
        let fake = Self::default();
        fake.state.borrow_mut().service_present = true;
        fake
    }

    /// A fake whose lookup fails.
    #[must_use]
    pub fn missing() -> Self { Self::default() }

    /// Queues the outcome of the next send.
    pub fn script(&self, outcome: SendOutcome) {
        self.state.borrow_mut().scripted.push_back(outcome);
    }

    /// Toggles service presence for later lookups.
    pub fn set_present(&self, present: bool) { self.state.borrow_mut().service_present = present; }

    /// The service disappears as soon as the next send is made, so the next
    /// lookup fails.
    pub fn vanish_on_send(&self) { self.state.borrow_mut().vanish_on_send = true; }

    /// Number of lookups performed.
    #[must_use]
    pub fn lookups(&self) -> usize { self.state.borrow().lookups }

    /// The sends performed, in order.
    #[must_use]
    pub fn sends(&self) -> Vec<FakeSend> { self.state.borrow().sends.clone() }
}

impl BordersTransport for FakeTransport {
    fn lookup(&mut self) -> Result<(), MissingService> {
        let mut state = self.state.borrow_mut();
        state.lookups += 1;
        if state.service_present {
            Ok(())
        } else {
            Err(MissingService { code: 1102 })
        }
    }

    fn send(&mut self, args: &[String], deadline: Duration) -> SendOutcome {
        let mut state = self.state.borrow_mut();
        state.sends.push(FakeSend { args: args.to_vec(), deadline });
        if state.vanish_on_send {
            state.service_present = false;
        }
        state.scripted.pop_front().unwrap_or(SendOutcome::Sent)
    }
}
