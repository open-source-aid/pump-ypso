use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use core::time::Duration;

use pump_shared::{
    Action, BleOp, BleResult, Class, Completion, CompletionError, Event, KeyRef, OpId, Secret, Slot,
    TherapyState, Timestamp, UtcTime,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    Ble,
    Stored,
    Loaded,
    Secret,
    Wrapped,
}

#[derive(Debug, Default)]
pub(crate) struct Io {
    last: u32,
    ready: Option<Action>,
    pending: Option<(OpId, Expect)>,
    reply: Option<(OpId, Completion)>,
    timer: Option<Timestamp>,
    pub(crate) now: Timestamp,
    pub(crate) wall: Option<(UtcTime, Timestamp)>,
    pub(crate) therapy: TherapyState,
    pub(crate) events: VecDeque<Event>,
    pub(crate) wrote: bool,
}

impl Io {
    pub(crate) fn poll_action(&mut self) -> Option<Action> {
        self.ready.take()
    }

    pub(crate) fn complete(
        &mut self,
        id: OpId,
        completion: Completion,
    ) -> Result<(), CompletionError> {
        let Some((pending, expect)) = self
            .pending
            .filter(|(p, _)| *p == id && self.ready.is_none())
        else {
            return Err(CompletionError::NotPending);
        };
        let fits = matches!(
            (expect, &completion),
            (_, Completion::Failed)
                | (Expect::Ble, Completion::Ble(_))
                | (Expect::Stored, Completion::Stored)
                | (Expect::Loaded, Completion::Loaded(_))
                | (Expect::Secret, Completion::Secret(_))
                | (Expect::Wrapped, Completion::Wrapped(_))
        );
        if !fits {
            return Err(CompletionError::Mismatch);
        }
        self.pending = None;
        self.reply = Some((pending, completion));
        Ok(())
    }

    pub(crate) fn timeout(&self) -> Option<Timestamp> {
        self.timer
    }

    pub(crate) fn is_waiting_on_host(&self) -> bool {
        self.pending.is_some() && self.ready.is_none()
    }

    pub(crate) fn withdraw(&mut self) {
        if self.ready.take().is_some() {
            self.pending = None;
        }
        self.timer = None;
        self.reply = None;
    }

    fn issue(&mut self, expect: Expect, action: impl FnOnce(OpId) -> Action) -> OpId {
        self.last = self.last.wrapping_add(1);
        let id = OpId::new(self.last);
        let action = action(id);
        self.wrote |= matches!(
            action,
            Action::Ble {
                op: BleOp::Write { .. },
                ..
            }
        );
        self.ready = Some(action);
        self.pending = Some((id, expect));
        id
    }
}

pub(crate) type Shared = alloc::rc::Rc<RefCell<Io>>;

struct Op<F> {
    io: Shared,
    expect: Expect,
    action: Option<F>,
    id: Option<OpId>,
}

impl<F: FnOnce(OpId) -> Action + Unpin> Future for Op<F> {
    type Output = Completion;

    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Completion> {
        let this = &mut *self;
        let mut io = this.io.borrow_mut();
        if let Some(action) = this.action.take() {
            this.id = Some(io.issue(this.expect, action));
        }
        match io.reply.take() {
            Some((id, completion)) if Some(id) == this.id => Poll::Ready(completion),
            other => {
                io.reply = other;
                Poll::Pending
            }
        }
    }
}

fn op<F: FnOnce(OpId) -> Action + Unpin>(io: &Shared, expect: Expect, action: F) -> Op<F> {
    Op {
        io: io.clone(),
        expect,
        action: Some(action),
        id: None,
    }
}

pub(crate) async fn ble(io: &Shared, op_: BleOp) -> BleResult {
    match op(io, Expect::Ble, move |id| Action::Ble { id, op: op_ }).await {
        Completion::Ble(result) => result,
        _ => BleResult::Failed,
    }
}

pub(crate) async fn store(io: &Shared, slot: Slot, class: Class, bytes: Vec<u8>) -> bool {
    matches!(
        op(io, Expect::Stored, move |id| Action::Store {
            id,
            slot,
            class,
            bytes
        })
        .await,
        Completion::Stored
    )
}

pub(crate) async fn erase(io: &Shared, slot: Slot) -> bool {
    matches!(
        op(io, Expect::Stored, move |id| Action::Erase { id, slot }).await,
        Completion::Stored
    )
}

pub(crate) async fn load(io: &Shared, slot: Slot) -> Result<Option<Vec<u8>>, ()> {
    match op(io, Expect::Loaded, move |id| Action::Load { id, slot }).await {
        Completion::Loaded(bytes) => Ok(bytes),
        _ => Err(()),
    }
}

pub(crate) async fn unwrap(io: &Shared, wrapped: Vec<u8>) -> Option<Secret> {
    match op(io, Expect::Secret, move |id| Action::Unwrap { id, wrapped }).await {
        Completion::Secret(secret) => Some(secret),
        _ => None,
    }
}

/// Seal a secret for storage at rest (the host's keystore wraps it). The offline pair uses this to
/// persist the freshly agreed session before it is written to `Slot::Session`.
pub(crate) async fn wrap(io: &Shared, plaintext: Secret) -> Option<Vec<u8>> {
    match op(io, Expect::Wrapped, move |id| Action::Wrap { id, plaintext }).await {
        Completion::Wrapped(bytes) => Some(bytes),
        _ => None,
    }
}

/// X25519 key agreement against a server key the host holds (the offline pair's secret the deeplink
/// carried). Returns the raw shared point; the engine derives the session key from it. Passing the
/// X25519 base point as `peer_public` yields the server key's own public key.
pub(crate) async fn key_agree(io: &Shared, key: KeyRef, peer_public: [u8; 32]) -> Option<[u8; 32]> {
    match op(io, Expect::Secret, move |id| Action::KeyAgree {
        id,
        key,
        peer_public,
    })
    .await
    {
        Completion::Secret(secret) if secret.expose().len() == 32 => {
            let mut out = [0u8; 32];
            out.copy_from_slice(secret.expose());
            Some(out)
        }
        _ => None,
    }
}

/// `len` random bytes from the platform's generator.
pub(crate) async fn random(io: &Shared, len: usize) -> Option<Secret> {
    match op(io, Expect::Secret, move |id| Action::Random { id, len }).await {
        Completion::Secret(secret) if secret.expose().len() == len => Some(secret),
        _ => None,
    }
}

struct Sleep {
    io: Shared,
    until: Timestamp,
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        let mut io = self.io.borrow_mut();
        if io.now >= self.until {
            io.timer = None;
            Poll::Ready(())
        } else {
            io.timer = Some(self.until);
            Poll::Pending
        }
    }
}

pub(crate) fn sleep(io: &Shared, duration: Duration) -> impl Future<Output = ()> {
    let now = io.borrow().now.as_millis();
    Sleep {
        io: io.clone(),
        until: Timestamp::from_millis(now + duration.as_millis() as u64),
    }
}
