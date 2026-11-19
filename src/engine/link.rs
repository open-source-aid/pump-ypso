use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use pump_shared::{Address, BleOp, BleResult, Class, CommandId, Failure, Outcome, Slot};

use super::io::{self, Shared};
use super::store::{Nonces, Session};
use crate::protocol::{setting, uuids, Frames, Reassembler, Service, Status, ERR_COUNTER_ERROR};
use crate::{Characteristic, Counters, PumpError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Halt {
    Unknown,
    #[cfg(feature = "delivery")]
    Refused(pump_shared::Refusal),
    Failed(Failure),
}

pub(crate) fn unknown(_: Halt) -> Halt {
    Halt::Unknown
}

impl From<Halt> for Outcome {
    fn from(halt: Halt) -> Self {
        match halt {
            Halt::Unknown => Self::Unknown,
            #[cfg(feature = "delivery")]
            Halt::Refused(refusal) => Self::Refused(refusal),
            Halt::Failed(failure) => Self::Failed(failure),
        }
    }
}

pub(crate) type Step<T> = Result<T, Halt>;

const MAX_COUNTER_SENDS: usize = 17;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteClass {
    Selection,
    Clock,
    Therapy,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Versions {
    pub(crate) pump: f64,
    pub(crate) master: String,
    pub(crate) history: f64,
    pub(crate) control: f64,
}

impl Versions {
    fn parse(text: &[u8]) -> (String, f64) {
        let text: String = text
            .iter()
            .map(|&b| char::from(b))
            .collect::<String>()
            .trim_matches(|c: char| c == '\0' || c.is_whitespace())
            .into();
        let number = text.parse().unwrap_or(0.0);
        (text, number)
    }

    pub(crate) fn extended(&self) -> bool {
        self.control > 1.1
    }

    fn known(&self) -> bool {
        self.pump >= 1.0
    }

    #[cfg(feature = "opn")]
    fn keyless_ok(&self) -> bool {
        self.known()
            && !self.extended()
            && !self.master.starts_with("V05")
            && !self.master.starts_with("V35")
    }
}

#[derive(Debug)]
pub(crate) struct Ctx {
    pub(crate) io: Shared,
    pub(crate) session: Session,
    pub(crate) counters: RefCell<Counters>,
    pub(crate) nonces: RefCell<Nonces>,
    pub(crate) link: RefCell<Option<Versions>>,
    pub(crate) device: RefCell<super::pump::Device>,
    pub(crate) ready: Cell<bool>,
    pub(crate) stale: Cell<bool>,
    pub(crate) unpaired: Cell<bool>,
    pub(crate) current: Cell<CommandId>,
    pub(crate) utc_offset: i32,
    pub(crate) variant: &'static str,
}

pub(crate) fn safevar(value: i32) -> Vec<u8> {
    [value.to_le_bytes(), (!value).to_le_bytes()].concat()
}

pub(crate) fn le_i32(bytes: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn failed(result: BleResult) -> Halt {
    match result {
        BleResult::Timeout => Halt::Unknown,
        BleResult::Disconnected => Halt::Failed(Failure::Disconnected),
        BleResult::Status(code) => status_failure(code),
        _ => Halt::Failed(Failure::Protocol),
    }
}

fn status_failure(code: u8) -> Halt {
    match PumpError::from_status(code) {
        Some(PumpError::EncryptionFailed | PumpError::NoSharedKey) => {
            Halt::Failed(Failure::KeyRejected)
        }
        _ => Halt::Failed(Failure::Status(code)),
    }
}

fn read_failed(result: BleResult) -> Halt {
    match failed(result) {
        Halt::Unknown => Halt::Failed(Failure::Disconnected),
        other => other,
    }
}

impl Ctx {
    pub(crate) fn new(
        io: Shared,
        session: Session,
        nonces: Nonces,
        utc_offset: i32,
        variant: &'static str,
    ) -> Self {
        let (write, read, epoch) = session.counters;
        Self {
            io,
            counters: RefCell::new(Counters::from_session(write, read, epoch)),
            nonces: RefCell::new(nonces),
            link: RefCell::new(None),
            device: RefCell::new(super::pump::Device::defaults()),
            ready: Cell::new(false),
            stale: Cell::new(false),
            current: Cell::new(CommandId::new(0)),
            unpaired: Cell::new(false),
            session,
            utc_offset,
            variant,
        }
    }

    pub(crate) fn firmware(&self) -> String {
        self.link
            .borrow()
            .as_ref()
            .map(|v| v.master.clone())
            .unwrap_or_default()
    }

    async fn ble(&self, op: BleOp) -> BleResult {
        let result = io::ble(&self.io, op).await;
        if matches!(result, BleResult::Disconnected | BleResult::Timeout) {
            self.link.replace(None);
            self.stale.set(result == BleResult::Timeout);
        }
        result
    }

    fn sealed(&self, characteristic: Characteristic) -> bool {
        let uuid = characteristic.uuid().unwrap_or(uuids::EXTENDED_READ);
        self.session.crypto.is_some()
            && match characteristic {
                Characteristic::GetBolusStatus => {
                    self.link.borrow().as_ref().is_some_and(Versions::extended)
                }
                _ => !uuids::is_cleartext_read(uuid) && !uuids::is_cleartext_write(uuid),
            }
    }

    async fn raw_read(&self, characteristic: Characteristic) -> Step<Vec<u8>> {
        self.raw_read_at(service_of(characteristic), uuid_of(characteristic))
            .await
    }

    async fn raw_read_at(
        &self,
        service: Service,
        characteristic: pump_shared::Uuid,
    ) -> Step<Vec<u8>> {
        match self
            .ble(BleOp::Read {
                service: service.uuid(),
                characteristic,
            })
            .await
        {
            BleResult::Data(bytes) => Ok(bytes),
            other => Err(read_failed(other)),
        }
    }

    pub(crate) async fn read(&self, characteristic: Characteristic) -> Step<Vec<u8>> {
        let first = self.raw_read(characteristic).await?;
        let Some(crypto) = self
            .session
            .crypto
            .as_ref()
            .filter(|_| self.sealed(characteristic))
        else {
            return Ok(first);
        };
        let mut rx = Reassembler::default();
        let mut message = rx.push(&first);
        while let Ok(None) = message {
            let frame = self
                .raw_read_at(Service::Crypto, uuids::EXTENDED_READ)
                .await?;
            message = rx.push(&frame);
        }
        let message = message
            .map_err(|_| Halt::Failed(Failure::Protocol))?
            .unwrap_or_default();
        let opened = crypto
            .open(&message)
            .map_err(|_| Halt::Failed(Failure::KeyRejected))?;
        let mut counters = self.counters.borrow_mut();
        counters.observe_reboot(opened.reboot, opened.counter);
        counters.accept_read(opened.counter);
        Ok(opened.payload)
    }

    pub(crate) async fn read_sized(
        &self,
        characteristic: Characteristic,
        len: usize,
    ) -> Step<Vec<u8>> {
        let bytes = self.read(characteristic).await?;
        if bytes.len() < len {
            return Err(Halt::Failed(Failure::Protocol));
        }
        Ok(bytes)
    }

    async fn raw_write(
        &self,
        characteristic: Characteristic,
        data: Vec<u8>,
    ) -> Result<(), BleResult> {
        match self
            .ble(BleOp::Write {
                service: service_of(characteristic).uuid(),
                characteristic: uuid_of(characteristic),
                data,
            })
            .await
        {
            BleResult::Done => Ok(()),
            other => Err(other),
        }
    }

    pub(crate) async fn write(
        &self,
        characteristic: Characteristic,
        class: WriteClass,
        payload: &[u8],
    ) -> Step<()> {
        let Some(crypto) = self
            .session
            .crypto
            .as_ref()
            .filter(|_| self.sealed(characteristic))
        else {
            return self
                .raw_write(characteristic, payload.to_vec())
                .await
                .map_err(failed);
        };
        let (sends, failure) = self.seed(class);
        for _ in 0..sends {
            let (counter, epoch, record) = {
                let counters = self.counters.borrow();
                let counter = counters.next_write().unwrap_or(u64::MAX);
                let mut used = counters.clone();
                used.accept_write(counter);
                (
                    counter,
                    counters.epoch(),
                    self.session.counters_record(&used),
                )
            };
            if !io::store(&self.io, Slot::Counters, Class::Monotonic, record).await {
                return Err(Halt::Failed(Failure::Host));
            }
            let nonce = self.nonce().await?;
            let sealed = crypto
                .seal(payload, epoch, counter, &nonce)
                .expect("a command always fits one sealed message");
            match self.send_frames(characteristic, &sealed).await {
                Ok(()) => {
                    self.counters.borrow_mut().accept_write(counter);
                    return Ok(());
                }
                Err(BleResult::Status(code)) if code == ERR_COUNTER_ERROR => {
                    self.read(Characteristic::GetSystemStatus).await?;
                    let mut counters = self.counters.borrow_mut();
                    if counters.epoch() == epoch && counters.on_counter_error().is_err() {
                        break;
                    }
                }
                Err(other) => return Err(failed(other)),
            }
        }
        Err(failure)
    }

    fn seed(&self, class: WriteClass) -> (usize, Halt) {
        let sends = (MAX_COUNTER_SENDS, status_failure(ERR_COUNTER_ERROR));
        let mut counters = self.counters.borrow_mut();
        if counters.next_write().is_ok() {
            return sends;
        }
        if class == WriteClass::Therapy {
            return (0, Halt::Failed(Failure::Protocol));
        }
        let seed = match counters.read_state() {
            crate::CounterState::Seeded(read) => read,
            crate::CounterState::Unseeded => 0,
        };
        counters.accept_write(seed);
        sends
    }

    async fn nonce(&self) -> Step<[u8; crate::protocol::NONCE_LEN]> {
        let reservation = self.nonces.borrow().reservation();
        if let Some(state) = reservation {
            if !io::store(&self.io, Slot::NonceState, Class::Monotonic, state).await {
                return Err(Halt::Failed(Failure::Host));
            }
            self.nonces.borrow_mut().reserved();
        }
        Ok(self.nonces.borrow_mut().take())
    }

    async fn send_frames(
        &self,
        characteristic: Characteristic,
        sealed: &[u8],
    ) -> Result<(), BleResult> {
        let frames = Frames::split(sealed).expect("a sealed command always fits the frames");
        let mut first = Ok(());
        for frame in frames {
            match self.raw_write(characteristic, frame).await {
                Ok(()) => {}
                Err(stop @ (BleResult::Timeout | BleResult::Disconnected)) => return Err(stop),
                Err(other) => first = first.and(Err(other)),
            }
        }
        first
    }

    pub(crate) async fn read_setting(&self, id: u16) -> Step<i32> {
        self.write(
            Characteristic::SettingId,
            WriteClass::Selection,
            &safevar(i32::from(id)),
        )
        .await?;
        Ok(le_i32(
            &self.read_sized(Characteristic::SettingValue, 4).await?,
            0,
        ))
    }

    #[cfg(feature = "delivery")]
    pub(crate) async fn write_setting(&self, id: u16, value: i32) -> Step<()> {
        self.write(
            Characteristic::SettingId,
            WriteClass::Selection,
            &safevar(i32::from(id)),
        )
        .await?;
        self.write(
            Characteristic::SettingValue,
            WriteClass::Therapy,
            &safevar(value),
        )
        .await
    }

    pub(crate) async fn connect(&self) -> Step<bool> {
        if self.link.borrow().is_some() {
            return Ok(false);
        }
        if self.stale.replace(false) {
            self.ble(BleOp::Disconnect).await;
        }
        let address = Address::new(self.session.mac);
        match self.ble(BleOp::Connect { address }).await {
            BleResult::Done => {}
            other => return Err(read_failed(other)),
        }
        self.ble(BleOp::RequestFastInterval).await;
        let versions = self.versions().await?;
        self.link.replace(Some(versions.clone()));
        let identified = self.identify(&versions).await;
        if identified.is_err() {
            self.link.replace(None);
            self.ble(BleOp::Disconnect).await;
        }
        identified.map(|()| true)
    }

    async fn versions(&self) -> Step<Versions> {
        let (_, pump) = Versions::parse(
            &self
                .raw_read(Characteristic::PumpBaseServiceVersion)
                .await?,
        );
        let (master, _) =
            Versions::parse(&self.raw_read(Characteristic::MasterSoftwareVersion).await?);
        self.raw_read(Characteristic::SettingsServiceVersion)
            .await?;
        let (_, history) =
            Versions::parse(&self.raw_read(Characteristic::HistoryServiceVersion).await?);
        let control = if pump >= 1.1 {
            Versions::parse(&self.raw_read(Characteristic::ControlServiceVersion).await?).1
        } else {
            0.0
        };
        Ok(Versions {
            pump,
            master,
            history,
            control,
        })
    }

    async fn identify(&self, versions: &Versions) -> Step<()> {
        self.authorise().await?;
        let lower = self.read_setting(setting::SERIAL_NUMBER_LOWER).await? as u32 & 0xFFFF;
        let upper = self.read_setting(setting::SERIAL_NUMBER_UPPER).await? as u32 & 0xFFFF;
        #[cfg(feature = "opn")]
        let refused = self.session.crypto.is_none() && !versions.keyless_ok();
        #[cfg(not(feature = "opn"))]
        let refused = false;
        if (upper << 16 | lower) != self.session.serial || !versions.known() || refused {
            return Err(Halt::Failed(Failure::Protocol));
        }
        Ok(())
    }

    async fn authorise(&self) -> Step<()> {
        let mut last = Ok(());
        for (code, password) in &self.session.passwords {
            last = match self
                .raw_write(Characteristic::AuthorizationPassword, password.to_vec())
                .await
            {
                Ok(()) => match code {
                    b'O' | b'S' | b'L' => {
                        self.read(Characteristic::GetSystemStatus).await.map(drop)
                    }
                    b'P' => self.read(Characteristic::AlarmEntryCount).await.map(drop),
                    _ => Ok(()),
                },
                Err(other) => Err(failed(other)),
            };
            match &last {
                Ok(()) => return Ok(()),
                Err(Halt::Failed(Failure::Status(code)))
                    if matches!(
                        Status::classify(*code),
                        Status::Att(
                            crate::AttError::InsufficientAuthentication
                                | crate::AttError::InsufficientEncryption
                        )
                    ) =>
                {
                    return last
                }
                Err(Halt::Failed(Failure::Disconnected)) => return last,
                Err(_) => {}
            }
        }
        last
    }

    pub(crate) async fn disconnect(&self) {
        if self.link.replace(None).is_some() {
            self.ble(BleOp::Disconnect).await;
        }
    }
}

fn service_of(characteristic: Characteristic) -> Service {
    characteristic.service().unwrap_or(Service::Crypto)
}

fn uuid_of(characteristic: Characteristic) -> pump_shared::Uuid {
    characteristic.uuid().unwrap_or(uuids::EXTENDED_READ)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::future::Future;
    use core::task::{Context, Poll, Waker};

    #[test]
    fn a_therapy_write_never_seeds_an_unknown_counter() {
        let session = Session::decode(&super::super::store::tests::blob(|_| {})).unwrap();
        let ctx = Ctx::new(
            Shared::default(),
            session,
            Nonces::resume([7; 16], None),
            0,
            "ypso",
        );
        let mut write =
            core::pin::pin!(ctx.write(Characteristic::StartStopBolus, WriteClass::Therapy, &[0]));
        let poll = write.as_mut().poll(&mut Context::from_waker(Waker::noop()));
        assert_eq!(poll, Poll::Ready(Err(Halt::Failed(Failure::Protocol))));
        assert!(ctx.io.borrow_mut().poll_action().is_none());
        let sends = (MAX_COUNTER_SENDS, status_failure(ERR_COUNTER_ERROR));
        assert_eq!(ctx.seed(WriteClass::Clock), sends);
        assert_eq!(ctx.counters.borrow().next_write(), Ok(6));
        assert_eq!(ctx.seed(WriteClass::Therapy), sends);
        ctx.counters.replace(Counters::from_session(-1, -1, 0));
        assert_eq!(ctx.seed(WriteClass::Selection), sends);
        assert_eq!(ctx.counters.borrow().next_write(), Ok(1));
    }
}
