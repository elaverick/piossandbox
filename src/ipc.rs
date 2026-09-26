//! Synchronous IPC through endpoints.
//!
//! An endpoint is a meeting point: a sender (or caller) and a receiver each
//! wait there until the other arrives, then the message passes directly
//! from one to the other. A call also leaves the caller waiting for a
//! reply, and gives the receiver a one-shot `ReplyCap` to send it with.
//!
//! Messages travel between threads in kernel form: the sender's thread
//! copies its message in from its own memory, and the receiver's thread
//! copies it out into its own memory and installs any handle it carries in
//! its own table. So each process's memory and handles are only ever
//! touched by its own thread.
//!
//! An endpoint counts the handles that can send to it and receive from it.
//! When the last of either kind is closed, whoever is waiting on the other
//! side can never be answered, so they are woken with `PeerGone`: as with
//! Rust's channels, dropping one end wakes the other.
//!
//! Anything that owns handles (a message, a reply capability) can run
//! arbitrary handle-closing code when dropped, including code that locks an
//! endpoint. So nothing here is dropped while an endpoint's lock is held.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use pios_abi::{Error, MESSAGE_WORDS, rights};

use crate::handle::Handle;
use crate::sync::SpinLock;
use crate::thread::{self, ThreadId};

/// A message on its way between threads.
pub struct Message {
    pub label: u64,
    pub data: [u64; MESSAGE_WORDS],
    /// A handle being moved to the receiver.
    pub handle: Option<Handle>,
}

/// What a receiver gets.
pub struct Delivery {
    pub message: Message,
    /// The badge of the handle the sender used.
    pub badge: u64,
    /// For a call, the way to answer it.
    pub reply: Option<ReplyCap>,
}

/// How a wait ended.
enum Outcome {
    /// (Receiver) a message arrived.
    Delivered(Delivery),
    /// (Sender) a receiver took the message.
    Sent,
    /// (Caller) the reply.
    Replied(Message),
    Failed(Error),
}

/// A thread waiting at an endpoint (or for a reply), and where its outcome
/// goes.
struct Waiter {
    thread: ThreadId,
    outcome: SpinLock<Option<Outcome>>,
}

impl Waiter {
    fn new() -> Arc<Waiter> {
        Arc::new(Waiter {
            thread: thread::current(),
            outcome: SpinLock::new(None),
        })
    }

    /// End the wait with `outcome`.
    fn complete(&self, outcome: Outcome) {
        let previous = self.outcome.lock().replace(outcome);
        assert!(previous.is_none(), "a wait ended twice");
        thread::unpark(self.thread);
    }

    /// Wait (as the thread that made this waiter) for the outcome.
    fn wait(&self) -> Outcome {
        loop {
            if let Some(outcome) = self.outcome.lock().take() {
                return outcome;
            }
            thread::park();
        }
    }
}

/// A sender waiting for a receiver.
struct Pending {
    waiter: Arc<Waiter>,
    message: Message,
    badge: u64,
    /// A call: the sender then waits for a reply.
    call: bool,
}

struct State {
    senders: VecDeque<Pending>,
    receivers: VecDeque<Arc<Waiter>>,
    /// Open handles with the `SEND` and `RECEIVE` rights.
    send_handles: usize,
    receive_handles: usize,
}

pub struct Endpoint {
    state: SpinLock<State>,
}

/// Endpoints in existence, for the self-test's leak check.
static LIVE_ENDPOINTS: AtomicUsize = AtomicUsize::new(0);

pub fn live_endpoints() -> usize {
    LIVE_ENDPOINTS.load(Ordering::Relaxed)
}

impl Endpoint {
    pub fn new() -> Arc<Endpoint> {
        LIVE_ENDPOINTS.fetch_add(1, Ordering::Relaxed);
        Arc::new(Endpoint {
            state: SpinLock::new(State {
                senders: VecDeque::new(),
                receivers: VecDeque::new(),
                send_handles: 0,
                receive_handles: 0,
            }),
        })
    }

    /// Send `message` (stamped with `badge`), waiting until a receiver takes
    /// it; for a `call`, then wait for the reply and return it.
    pub fn send(&self, message: Message, badge: u64, call: bool) -> Result<Option<Message>, Error> {
        let waiter = Waiter::new();
        let mut state = self.state.lock();
        if state.receive_handles == 0 {
            drop(state);
            // (Dropping the message closes any handle in it.)
            return Err(Error::PeerGone);
        }
        match state.receivers.pop_front() {
            Some(receiver) => {
                drop(state);
                let reply = call.then(|| ReplyCap::new(waiter.clone()));
                receiver.complete(Outcome::Delivered(Delivery {
                    message,
                    badge,
                    reply,
                }));
                if !call {
                    return Ok(None);
                }
            }
            None => {
                state.senders.push_back(Pending {
                    waiter: waiter.clone(),
                    message,
                    badge,
                    call,
                });
                drop(state);
            }
        }
        match waiter.wait() {
            Outcome::Sent => Ok(None),
            Outcome::Replied(reply) => Ok(Some(reply)),
            Outcome::Failed(error) => Err(error),
            Outcome::Delivered(_) => unreachable!("a sender was given a message"),
        }
    }

    /// Wait for a message.
    pub fn receive(&self) -> Result<Delivery, Error> {
        let mut state = self.state.lock();
        if let Some(pending) = state.senders.pop_front() {
            drop(state);
            let reply = if pending.call {
                Some(ReplyCap::new(pending.waiter))
            } else {
                pending.waiter.complete(Outcome::Sent);
                None
            };
            return Ok(Delivery {
                message: pending.message,
                badge: pending.badge,
                reply,
            });
        }
        if state.send_handles == 0 {
            return Err(Error::PeerGone);
        }
        let waiter = Waiter::new();
        state.receivers.push_back(waiter.clone());
        drop(state);
        match waiter.wait() {
            Outcome::Delivered(delivery) => Ok(delivery),
            Outcome::Failed(error) => Err(error),
            _ => unreachable!("a receiver was not given a message"),
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        LIVE_ENDPOINTS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The one way to answer a call. Dropping it unanswered tells the caller
/// the server is gone.
pub struct ReplyCap {
    caller: Option<Arc<Waiter>>,
}

impl ReplyCap {
    fn new(caller: Arc<Waiter>) -> ReplyCap {
        ReplyCap {
            caller: Some(caller),
        }
    }

    pub fn reply(mut self, message: Message) {
        let caller = self.caller.take().expect("not yet replied");
        caller.complete(Outcome::Replied(message));
    }
}

impl Drop for ReplyCap {
    fn drop(&mut self) {
        if let Some(caller) = self.caller.take() {
            caller.complete(Outcome::Failed(Error::PeerGone));
        }
    }
}

/// An endpoint as a handle refers to it: with rights and a badge. Creating
/// and dropping these keeps the endpoint's handle counts.
pub struct EndpointRef {
    endpoint: Arc<Endpoint>,
    rights: usize,
    badge: u64,
}

impl EndpointRef {
    /// A reference with `rights` (endpoint rights only) and `badge`.
    pub fn new(endpoint: Arc<Endpoint>, rights: usize, badge: u64) -> EndpointRef {
        assert_eq!(rights & !rights::ENDPOINT_ALL, 0, "not endpoint rights");
        {
            let mut state = endpoint.state.lock();
            if rights & rights::SEND != 0 {
                state.send_handles += 1;
            }
            if rights & rights::RECEIVE != 0 {
                state.receive_handles += 1;
            }
        }
        EndpointRef {
            endpoint,
            rights,
            badge,
        }
    }

    pub fn endpoint(&self) -> &Arc<Endpoint> {
        &self.endpoint
    }

    pub fn rights(&self) -> usize {
        self.rights
    }

    pub fn badge(&self) -> u64 {
        self.badge
    }
}

impl Drop for EndpointRef {
    fn drop(&mut self) {
        let (senders, receivers) = {
            let mut state = self.endpoint.state.lock();
            let mut senders = Vec::new();
            let mut receivers = Vec::new();
            if self.rights & rights::SEND != 0 {
                state.send_handles -= 1;
                if state.send_handles == 0 {
                    receivers.extend(state.receivers.drain(..));
                }
            }
            if self.rights & rights::RECEIVE != 0 {
                state.receive_handles -= 1;
                if state.receive_handles == 0 {
                    senders.extend(state.senders.drain(..));
                }
            }
            (senders, receivers)
        };
        // Wake them without the lock: dropping their messages may close
        // handles to this same endpoint.
        for receiver in receivers {
            receiver.complete(Outcome::Failed(Error::PeerGone));
        }
        for pending in senders {
            pending.waiter.complete(Outcome::Failed(Error::PeerGone));
        }
    }
}
