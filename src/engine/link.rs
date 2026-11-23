use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use pump_shared::{
    Address, BleOp, BleResult, Class, CommandId, Failure, KeyRef, Outcome, Refusal, Report, Secret,
    Slot,
};

use super::crc::crc32_update;
use super::io::{self, Shared};
use super::store::{Nonces, Session};
use crate::protocol::{
    setting, uuids, Crypto, Frames, Reassembler, Service, Status, ERR_COUNTER_ERROR, KEY_LEN,
    NONCE_LEN,
};
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

// ===== Offline pair (package YPSO-OFFLINE-PAIR) ==================================================
//
// The pump's key exchange, done on this side instead of by the vendor's backend. After the usual
// AuthorizationPassword, read NewPublicKey (a 32-byte challenge, then the pump's 32-byte X25519
// public key), then write an authorisation blob to WriteChallengeUtcAppPubKey:
//   XChaCha20-Poly1305(K_s, nonce)(challenge ‖ ttl u32 LE ‖ server public key ‖ counter u64 LE) ‖ nonce
// with K_s = HChaCha20(X25519(server private, pump public), 0^16). The pump opens the blob with the
// server public key it trusts and takes the blob's public-key field as the other side of its new
// session, so the key the pump ends up holding is K_s itself.
//
// The server private key never reaches the engine: the host performs the X25519 agreement
// (Action::KeyAgree), both for K_s and — against the X25519 base point — for the server's own public
// key that goes in the blob. The engine only derives (HChaCha20) and seals (XChaCha20-Poly1305).

const PAIR_CHALLENGE_LEN: usize = 32;
const PAIR_OFFER_LEN: usize = PAIR_CHALLENGE_LEN + KEY_LEN;
const PAIR_TAG_LEN: usize = 16;
const PAIR_PLAINTEXT_LEN: usize = PAIR_CHALLENGE_LEN + 4 + KEY_LEN + 8;
const PAIR_BLOB_LEN: usize = PAIR_PLAINTEXT_LEN + PAIR_TAG_LEN + NONCE_LEN;
/// The validity the vendor's own backend asks for (28 days, in seconds).
const PAIR_TTL_SECS: u32 = 28 * 24 * 60 * 60;
/// The pump's post-exchange cooldown (~5 min) and the ~45 s not-ready after power-on: a read that
/// answers either right after the write means the pump took the key, not that it refused it.
const ERR_BLOCKED: u8 = 0x88;
const ERR_NOT_READY: u8 = 0x89;

struct PumpOffer {
    challenge: [u8; PAIR_CHALLENGE_LEN],
    pump_public: [u8; KEY_LEN],
}

/// `HChaCha20(input, 0^16)`, the derivation both sides use.
fn hchacha20(input: &[u8; 32]) -> [u8; 32] {
    use chacha20::cipher::consts::U10;
    chacha20::hchacha::<U10>(input.into(), &[0u8; 16].into()).into()
}

/// Seal `plaintext` as `ciphertext ‖ tag ‖ nonce` under `k_s` with XChaCha20-Poly1305.
fn seal_blob(k_s: &[u8; 32], nonce: &[u8; NONCE_LEN], plaintext: &[u8]) -> Option<Vec<u8>> {
    use chacha20poly1305::aead::{AeadInPlace, KeyInit};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};
    let cipher = XChaCha20Poly1305::new(k_s.into());
    let mut buf = plaintext.to_vec();
    let tag = cipher
        .encrypt_in_place_detached(XNonce::from_slice(nonce), &[], &mut buf)
        .ok()?;
    let mut blob = Vec::with_capacity(PAIR_BLOB_LEN);
    blob.extend_from_slice(&buf);
    blob.extend_from_slice(&tag);
    blob.extend_from_slice(nonce);
    Some(blob)
}

/// A counter strictly above any the pump can already hold: milliseconds since 1970 when the wall
/// clock is known (a vendor-keyed pump holds a large one), else 1 (a pump counting from zero).
fn pair_counter(io: &Shared) -> u64 {
    let io = io.borrow();
    io.wall
        .map(|(wall, at)| {
            wall.as_unix_millis() + io.now.as_millis().saturating_sub(at.as_millis())
        })
        .filter(|&c| c > 0)
        .unwrap_or(1)
}

fn install_key(ctx: &mut Ctx, key: [u8; KEY_LEN], prefix: [u8; 16]) {
    ctx.session.crypto = Some(Crypto::new(key));
    ctx.session.prefix = prefix;
    ctx.session.fingerprint = (!crc32_update(crc32_update(!0, b"ypso counters key"), &key)).max(1);
    ctx.session.counters = (-1, -1, 0);
    *ctx.counters.borrow_mut() = Counters::from_session(-1, -1, 0);
    *ctx.nonces.borrow_mut() = Nonces::resume(prefix, None);
}

async fn store_session(io: &Shared, ctx: &Ctx, key: &[u8; KEY_LEN]) -> bool {
    let bytes = ctx.session.encode(key);
    let Some(wrapped) = io::wrap(io, Secret::new(bytes)).await else {
        return false;
    };
    io::store(io, Slot::Session, Class::Secret, wrapped).await
}

/// The provisioning record (serial and credentials) kept in the store. pump-yspo's `Command::Pair`
/// carries only the address and the server key, so the serial and the passwords needed to open the
/// pump's offer come from the session blob already provisioned for it.
async fn provisioning(io: &Shared) -> Option<Session> {
    let wrapped = io::load(io, Slot::Session).await.ok()??;
    let secret = io::unwrap(io, wrapped).await?;
    Session::decode(secret.expose())
}

impl Ctx {
    /// Connect and read the service versions for the offline pair. No identify: the serial
    /// characteristic is sealed, unreadable before the key, and the keyless refusal does not apply
    /// to a pump being keyed.
    async fn pair_connect(&self) -> Step<()> {
        let address = Address::new(self.session.mac);
        match self.ble(BleOp::Connect { address }).await {
            BleResult::Done => {}
            other => return Err(read_failed(other)),
        }
        self.ble(BleOp::RequestFastInterval).await;
        let versions = self.versions().await?;
        self.link.replace(Some(versions));
        Ok(())
    }

    /// Present each credential until the pump hands over its key offer. The offer read is the
    /// probe: a credential the pump refuses fails the read, and the walk moves to the next.
    async fn pair_offer(&self, passwords: &[(u8, [u8; 16])]) -> Step<PumpOffer> {
        if passwords.is_empty() {
            return self.read_offer().await;
        }
        let mut last = Err(Halt::Failed(Failure::Protocol));
        for (_, password) in passwords {
            if let Err(other) = self
                .raw_write(Characteristic::AuthorizationPassword, password.to_vec())
                .await
            {
                last = Err(failed(other));
                continue;
            }
            match self.read_offer().await {
                Ok(offer) => return Ok(offer),
                Err(e @ Halt::Failed(Failure::Disconnected)) => return Err(e),
                Err(e) => last = Err(e),
            }
        }
        last
    }

    /// Read NewPublicKey, reassembling continuations over ExtendedRead. Cleartext: the offer
    /// carries the material the key is agreed from, so it is never sealed.
    async fn read_offer(&self) -> Step<PumpOffer> {
        let first = self
            .raw_read_at(Service::Crypto, uuids::NEW_PUBLIC_KEY)
            .await?;
        let mut rx = Reassembler::default();
        let mut message = rx.push(&first);
        while let Ok(None) = message {
            let frame = self
                .raw_read_at(Service::Crypto, uuids::EXTENDED_READ)
                .await?;
            message = rx.push(&frame);
        }
        let bytes = message
            .map_err(|_| Halt::Failed(Failure::Protocol))?
            .unwrap_or_default();
        if bytes.len() != PAIR_OFFER_LEN {
            return Err(Halt::Failed(Failure::Protocol));
        }
        let mut challenge = [0u8; PAIR_CHALLENGE_LEN];
        let mut pump_public = [0u8; KEY_LEN];
        challenge.copy_from_slice(&bytes[..PAIR_CHALLENGE_LEN]);
        pump_public.copy_from_slice(&bytes[PAIR_CHALLENGE_LEN..]);
        Ok(PumpOffer {
            challenge,
            pump_public,
        })
    }

    /// Write the authorisation blob's frames to WriteChallengeUtcAppPubKey (a cleartext write).
    async fn pair_write_blob(&self, blob: &[u8]) -> Step<()> {
        let frames = Frames::split(blob).map_err(|_| Halt::Failed(Failure::Protocol))?;
        for frame in frames {
            match self
                .ble(BleOp::Write {
                    service: uuids::CRYPTO_SERVICE,
                    characteristic: uuids::WRITE_CHALLENGE_UTC_APP_PUB_KEY,
                    data: frame,
                })
                .await
            {
                BleResult::Done => {}
                other => return Err(failed(other)),
            }
        }
        Ok(())
    }

    /// Prove the pump holds the new key by reading the system status under it.
    async fn pair_verify(&self) -> Step<()> {
        self.read(Characteristic::GetSystemStatus).await.map(drop)
    }
}

/// Run one offline pair. Returns the outcome and, when the pump may now hold the agreed key, the
/// live [`Ctx`] on it. The server private key stays with the host (`Action::KeyAgree`); nothing
/// secret is logged.
pub(crate) async fn offline_pair(
    io: Shared,
    address: Address,
    server_key: Option<KeyRef>,
    utc_offset: i32,
    variant: &'static str,
) -> (Outcome, Option<Ctx>) {
    let Some(server_key) = server_key else {
        return (Outcome::Refused(Refusal::NotPaired), None);
    };
    let Some(provision) = provisioning(&io).await else {
        return (Outcome::Refused(Refusal::NotPaired), None);
    };
    // An OPN pump has no CryptoService and no key to exchange: its provisioning record is keyless.
    // Pairing is not applicable, so refuse cleanly (never by failing to read a missing offer).
    if provision.crypto.is_none() {
        return (Outcome::Refused(Refusal::Unsupported), None);
    }
    let passwords = provision.passwords.clone();
    let mut ctx = Ctx::new(
        io.clone(),
        Session {
            mac: address.to_bytes(),
            serial: provision.serial,
            crypto: None,
            fingerprint: 1,
            counters: (-1, -1, 0),
            prefix: [1u8; 16],
            passwords: passwords.clone(),
        },
        Nonces::resume([1u8; 16], None),
        utc_offset,
        variant,
    );

    let prepared = async {
        ctx.pair_connect().await?;
        let offer = ctx.pair_offer(&passwords).await?;
        // The server's public key for the blob's field: the same agreement against the base point.
        let mut basepoint = [0u8; 32];
        basepoint[0] = 9;
        let server_public = io::key_agree(&io, server_key.clone(), basepoint)
            .await
            .ok_or(Halt::Failed(Failure::KeyRejected))?;
        let shared = io::key_agree(&io, server_key.clone(), offer.pump_public)
            .await
            .ok_or(Halt::Failed(Failure::KeyRejected))?;
        let k_s = hchacha20(&shared);
        let nonce_bytes = io::random(&io, NONCE_LEN)
            .await
            .ok_or(Halt::Failed(Failure::Host))?;
        let prefix_bytes = io::random(&io, 16).await.ok_or(Halt::Failed(Failure::Host))?;
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(nonce_bytes.expose());
        let mut prefix = [0u8; 16];
        prefix.copy_from_slice(prefix_bytes.expose());
        if prefix == [0u8; 16] {
            prefix[0] = 1;
        }
        let mut pt = [0u8; PAIR_PLAINTEXT_LEN];
        pt[..32].copy_from_slice(&offer.challenge);
        pt[32..36].copy_from_slice(&PAIR_TTL_SECS.to_le_bytes());
        pt[36..68].copy_from_slice(&server_public);
        pt[68..76].copy_from_slice(&pair_counter(&io).to_le_bytes());
        let blob = seal_blob(&k_s, &nonce, &pt).ok_or(Halt::Failed(Failure::Protocol))?;
        Ok::<_, Halt>((k_s, prefix, blob))
    }
    .await;

    let (k_s, prefix, blob) = match prepared {
        Ok(v) => v,
        Err(halt) => {
            ctx.disconnect().await;
            return (halt.into(), None);
        }
    };

    match ctx.pair_write_blob(&blob).await {
        Ok(()) => {
            // Accepted: install the key, persist the session, then prove it.
            install_key(&mut ctx, k_s, prefix);
            if !store_session(&io, &ctx, &k_s).await {
                ctx.disconnect().await;
                return (Outcome::Failed(Failure::Host), None);
            }
            match ctx.pair_verify().await {
                Ok(()) => (Outcome::Completed(Report::Done), Some(ctx)),
                // The post-pair cooldown / not-ready: the pump took the key, proven at a later read.
                Err(Halt::Failed(Failure::Status(ERR_BLOCKED | ERR_NOT_READY))) => {
                    (Outcome::Completed(Report::Done), Some(ctx))
                }
                Err(Halt::Failed(Failure::KeyRejected)) => {
                    let _ = io::erase(&io, Slot::Session).await;
                    ctx.disconnect().await;
                    (Outcome::Failed(Failure::KeyRejected), None)
                }
                // The link went before the proof: the pump may hold it; keep it, settle next connect.
                Err(_) => (Outcome::Unknown, Some(ctx)),
            }
        }
        // The pump answered the write with a refusal: it kept its old key; nothing is persisted.
        Err(Halt::Failed(failure @ (Failure::Status(_) | Failure::KeyRejected))) => {
            ctx.disconnect().await;
            (Outcome::Failed(failure), None)
        }
        // The write did not complete: the pump may already hold the new key; keep it.
        Err(_) => {
            install_key(&mut ctx, k_s, prefix);
            if store_session(&io, &ctx, &k_s).await {
                (Outcome::Unknown, Some(ctx))
            } else {
                ctx.disconnect().await;
                (Outcome::Failed(Failure::Host), None)
            }
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
