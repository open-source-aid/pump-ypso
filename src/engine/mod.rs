mod commands;
pub(crate) mod crc;
#[cfg(feature = "delivery")]
mod delivery;
mod history;
mod io;
mod link;
mod pump;
mod store;

pub use store::{inspect_session, SessionError, SessionSummary};

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use pump_shared::{
    Action, Command, CommandId, Completion, CompletionError, Config, Engine, EngineState, Event,
    Failure, Notification, Outcome, Refusal, Report, Slot, SubmitError, TherapyMachine,
    TherapyState, Timestamp, Transition, UtcTime,
};

use io::Shared;
use link::Ctx;
use store::{Nonces, Session};

const QUEUE_LEN: usize = 4;

type Task<T> = Pin<Box<dyn Future<Output = T>>>;

enum Running {
    Load(Task<Option<Ctx>>),
    Command(CommandId, Task<Outcome>),
    Stop(CommandId, Task<Outcome>),
}

pub(crate) struct YpsoEngine {
    io: Shared,
    ctx: Option<Rc<Ctx>>,
    running: Option<Running>,
    queue: VecDeque<(CommandId, Command)>,
    last_id: u32,
    stop: Option<CommandId>,
    therapy: TherapyState,
    state: EngineState,
}

impl YpsoEngine {
    pub(crate) fn new(variant: &'static str, config: Config) -> Self {
        let io = Shared::default();
        let task = load(io.clone(), variant, config.utc_offset_secs);
        let mut engine = Self {
            io,
            ctx: None,
            running: Some(Running::Load(Box::pin(task))),
            queue: VecDeque::new(),
            last_id: 0,
            stop: None,
            therapy: TherapyState::default(),
            state: EngineState::Loading,
        };
        engine.run();
        engine
    }

    fn next_id(&mut self) -> CommandId {
        self.last_id += 1;
        CommandId::new(self.last_id)
    }

    fn event(&self, event: Event) {
        self.io.borrow_mut().events.push_back(event);
    }

    fn finish(&mut self, id: CommandId, outcome: Outcome) {
        if outcome == Outcome::Failed(Failure::KeyRejected) {
            self.set_state(EngineState::KeyRejected);
        }
        self.event(Event::Finished { id, outcome });
    }

    fn set_state(&mut self, state: EngineState) {
        if self.state != state {
            self.state = state;
            self.event(Event::StateChanged(state));
        }
    }

    fn start(&mut self) {
        let Some(ctx) = self.ctx.clone() else {
            if let Some(id) = self.stop.take() {
                self.finish(id, Outcome::Refused(Refusal::NotPaired));
            }
            while let Some((id, _)) = self.queue.pop_front() {
                self.finish(id, Outcome::Refused(Refusal::NotPaired));
            }
            return;
        };
        self.io.borrow_mut().wrote = false;
        if let Some(id) = self.stop {
            self.event(Event::Started { id });
            ctx.current.set(id);
            self.running = Some(Running::Stop(
                id,
                Box::pin(async move { ctx.stop_bolus().await }),
            ));
        } else if let Some((id, command)) = self.queue.pop_front() {
            self.event(Event::Started { id });
            ctx.current.set(id);
            let task: Task<Outcome> = if command == Command::Unpair {
                Box::pin(unpair(ctx))
            } else {
                Box::pin(async move { ctx.run(command).await })
            };
            self.running = Some(Running::Command(id, task));
        }
    }

    fn preempt(&mut self) {
        let preempted = self.stop.is_some() && !self.io.borrow().is_waiting_on_host();
        if let (true, Some(Running::Command(id, _))) = (preempted, &self.running) {
            let id = *id;
            self.running = None;
            let mut io = self.io.borrow_mut();
            io.withdraw();
            let outcome = if io.wrote {
                Outcome::Unknown
            } else {
                Outcome::Refused(Refusal::Busy)
            };
            drop(io);
            self.finish(id, outcome);
        }
    }

    fn run(&mut self) {
        self.io.borrow_mut().therapy = self.therapy;
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            self.preempt();
            if self.running.is_none() && self.state != EngineState::Loading {
                self.start();
            }
            let progressed = match self.running.as_mut() {
                None => false,
                Some(Running::Load(task)) => match task.as_mut().poll(&mut cx) {
                    Poll::Ready(ctx) => {
                        self.running = None;
                        self.loaded(ctx);
                        true
                    }
                    Poll::Pending => false,
                },
                Some(Running::Command(id, task) | Running::Stop(id, task)) => {
                    let id = *id;
                    match task.as_mut().poll(&mut cx) {
                        Poll::Ready(outcome) => {
                            self.ended(id, outcome);
                            true
                        }
                        Poll::Pending => false,
                    }
                }
            };
            if !progressed {
                break;
            }
        }
        self.therapy = self.io.borrow().therapy;
    }

    fn loaded(&mut self, ctx: Option<Ctx>) {
        let state = if ctx.is_some() {
            EngineState::Paired
        } else {
            EngineState::Unpaired
        };
        self.ctx = ctx.map(Rc::new);
        self.set_state(state);
    }

    fn ended(&mut self, id: CommandId, outcome: Outcome) {
        if matches!(self.running, Some(Running::Stop(..))) {
            self.stop = None;
        }
        if matches!(self.running, Some(Running::Command(..)))
            && outcome == Outcome::Completed(Report::Done)
            && self.ctx.as_ref().is_some_and(|ctx| ctx.unpaired.get())
        {
            self.ctx = None;
            self.set_state(EngineState::Unpaired);
        }
        self.running = None;
        self.finish(id, outcome);
    }
}

async fn load(io: Shared, variant: &'static str, utc_offset: i32) -> Option<Ctx> {
    let wrapped = io::load(&io, Slot::Session).await.ok()??;
    let secret = io::unwrap(&io, wrapped).await?;
    let mut session = Session::decode(secret.expose())?;
    session.resume(io::load(&io, Slot::Counters).await.ok()?.as_deref());
    let nonces = Nonces::resume(
        session.prefix,
        io::load(&io, Slot::NonceState).await.ok()?.as_deref(),
    );
    Some(Ctx::new(io, session, nonces, utc_offset, variant))
}

async fn unpair(ctx: Rc<Ctx>) -> Outcome {
    ctx.disconnect().await;
    for slot in [Slot::Session, Slot::Counters, Slot::NonceState] {
        if !io::erase(&ctx.io, slot).await {
            return Outcome::Failed(Failure::Host);
        }
    }
    ctx.unpaired.set(true);
    Outcome::Completed(Report::Done)
}

impl TherapyMachine for YpsoEngine {
    fn therapy(&self) -> &TherapyState {
        &self.therapy
    }

    fn therapy_mut(&mut self) -> &mut TherapyState {
        &mut self.therapy
    }
}

impl Engine for YpsoEngine {
    fn submit(&mut self, command: Command) -> Result<CommandId, SubmitError> {
        if self.queue.len() >= QUEUE_LEN {
            return Err(SubmitError::QueueFull);
        }
        let id = self.next_id();
        self.queue.push_back((id, command));
        self.run();
        Ok(id)
    }

    fn stop(&mut self) -> CommandId {
        let id = match self.stop {
            Some(id) => id,
            None => {
                let id = self.next_id();
                self.stop = Some(id);
                id
            }
        };
        while let Some((dropped, _)) = self.queue.pop_front() {
            self.finish(dropped, Outcome::Refused(Refusal::Busy));
        }
        self.run();
        id
    }

    fn handle_completion(
        &mut self,
        id: pump_shared::OpId,
        completion: Completion,
    ) -> Result<(), CompletionError> {
        self.io.borrow_mut().complete(id, completion)?;
        self.run();
        Ok(())
    }

    fn handle_timeout(&mut self, now: Timestamp) {
        let mut io = self.io.borrow_mut();
        io.now = io.now.max(now);
        drop(io);
        self.run();
    }

    fn handle_wall_clock(&mut self, now: UtcTime) {
        let mut io = self.io.borrow_mut();
        io.wall = Some((now, io.now));
    }

    fn handle_link_lost(&mut self) {
        if let Some(ctx) = &self.ctx {
            ctx.link.replace(None);
        }
        let _ = self.therapy.apply(Transition::LinkLost);
        self.run();
    }

    fn handle_notification(&mut self, _notification: Notification) {}

    fn poll_action(&mut self) -> Option<Action> {
        self.io.borrow_mut().poll_action()
    }

    fn poll_event(&mut self) -> Option<Event> {
        self.io.borrow_mut().events.pop_front()
    }

    fn poll_timeout(&self) -> Option<Timestamp> {
        self.io.borrow().timeout()
    }

    fn state(&self) -> EngineState {
        self.state
    }
}
