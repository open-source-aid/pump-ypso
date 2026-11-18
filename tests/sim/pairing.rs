use chacha20::cipher::consts::U10;
use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use curve25519_dalek::montgomery::MontgomeryPoint;
use pump_shared::{BleResult, KeyRef};
use pump_ypso::protocol::{uuids, Characteristic, KEY_LEN};
use pump_ypso::sim::YpsoSim;
use pump_ypso::{Crypto, Frames};
use tester_harness::SimDevice;

use super::{read, write, App, KEY, PASSWORD};

const SERVER: [u8; KEY_LEN] = [0x11; KEY_LEN];
const APP: [u8; KEY_LEN] = [0x33; KEY_LEN];
const TTL: u32 = 28 * 24 * 60 * 60;

fn public(secret: [u8; KEY_LEN]) -> [u8; KEY_LEN] {
    MontgomeryPoint::mul_base_clamped(secret).to_bytes()
}

fn derive(secret: [u8; KEY_LEN], peer: [u8; KEY_LEN]) -> [u8; KEY_LEN] {
    let shared = MontgomeryPoint(peer).mul_clamped(secret).to_bytes();
    chacha20::hchacha::<U10>(&shared.into(), &[0u8; 16].into()).into()
}

fn blob(server: [u8; KEY_LEN], offer: &[u8], field: [u8; KEY_LEN], counter: u64) -> Vec<u8> {
    let pump_public: [u8; KEY_LEN] = offer[32..].try_into().unwrap();
    let mut plaintext = offer[..32].to_vec();
    plaintext.extend_from_slice(&TTL.to_le_bytes());
    plaintext.extend_from_slice(&field);
    plaintext.extend_from_slice(&counter.to_le_bytes());
    let nonce = [0x66; 24];
    let tag = XChaCha20Poly1305::new(&derive(server, pump_public).into())
        .encrypt_in_place_detached(XNonce::from_slice(&nonce), &[], &mut plaintext)
        .unwrap();
    [plaintext, tag.to_vec(), nonce.to_vec()].concat()
}

fn offer(app: &mut App) -> Vec<u8> {
    app.reassemble(uuids::CRYPTO_SERVICE, uuids::NEW_PUBLIC_KEY)
        .unwrap()
}

fn exchange(app: &mut App, blob: &[u8]) -> BleResult {
    let results: Vec<BleResult> = Frames::split(blob)
        .unwrap()
        .into_iter()
        .map(|frame| {
            app.op(write(
                uuids::CRYPTO_SERVICE,
                uuids::WRITE_CHALLENGE_UTC_APP_PUB_KEY,
                &frame,
            ))
        })
        .collect();
    assert_eq!(results.len(), 7);
    assert!(results[..6].iter().all(|r| *r == BleResult::Done));
    results[6].clone()
}

#[test]
fn the_offer_is_the_challenge_then_the_pump_key_in_clear_frames() {
    let mut app = App::connected();
    let frames = app
        .read_frames(uuids::CRYPTO_SERVICE, uuids::NEW_PUBLIC_KEY)
        .unwrap();
    assert_eq!(frames.len(), 4);
    let offer = offer(&mut app);
    assert_eq!(offer[..32], app.sim.challenge);
    assert_eq!(offer[32..], app.sim.pump_public());
    assert_eq!(offer[32..], public(app.sim.pump_secret));
    assert_eq!(app.sim.pump_counter, 0);
}

#[test]
fn the_offer_and_the_exchange_need_the_password_first() {
    let mut app = App::new(YpsoSim::new(KEY, PASSWORD));
    assert_eq!(
        app.op(read(uuids::CRYPTO_SERVICE, uuids::NEW_PUBLIC_KEY)),
        BleResult::Status(0x08)
    );
    let offer = [app.sim.challenge, app.sim.pump_public()].concat();
    assert_eq!(
        exchange(&mut app, &blob(SERVER, &offer, public(SERVER), 1)),
        BleResult::Status(0x08)
    );
    assert_eq!(app.sim.key, Some(KEY));
}

#[test]
fn a_blob_from_the_trusted_server_sets_the_new_session_key_then_the_pump_cools_down() {
    let mut app = App::connected();
    let offer = offer(&mut app);
    let counter = 1_763_000_000_000;
    assert_eq!(
        exchange(&mut app, &blob(SERVER, &offer, public(SERVER), counter)),
        BleResult::Done
    );
    let pump_public: [u8; KEY_LEN] = offer[32..].try_into().unwrap();
    let session = derive(SERVER, pump_public);
    assert_eq!(app.sim.key, Some(session));
    assert_eq!(app.sim.pairing_counter, counter);
    let shared = app
        .sim
        .key_agree(&KeyRef::new("server"), &pump_public)
        .unwrap();
    assert_eq!(
        <[u8; 32]>::from(chacha20::hchacha::<U10>(&shared.into(), &[0u8; 16].into())),
        session
    );
    assert_eq!(
        (app.sim.reboot, app.sim.pump_counter, app.sim.app_counter),
        (1, 0, 0)
    );
    app.crypto = Some(Crypto::new(session));
    app.reboot = 1;
    app.counter = 0;
    assert_eq!(
        app.get(Characteristic::GetSystemStatus),
        Err(BleResult::Status(0x88))
    );
    assert_eq!(
        app.op(read(uuids::CRYPTO_SERVICE, uuids::NEW_PUBLIC_KEY)),
        BleResult::Status(0x88)
    );
    app.wait(299_999);
    assert_eq!(
        app.send(Characteristic::SettingId, &super::safevar(62)),
        BleResult::Status(0x88)
    );
    app.wait(1);
    assert_eq!(
        app.opened(Characteristic::GetSystemStatus).unwrap().counter,
        1
    );
    assert_eq!(
        app.send(Characteristic::SettingId, &super::safevar(62)),
        BleResult::Done
    );
}

#[test]
fn no_offer_while_the_pump_is_not_ready_or_holds_no_session() {
    let mut app = App::connected();
    app.sim.reboot(app.at());
    assert_eq!(app.op(super::connect()), BleResult::Done);
    assert_eq!(app.authorise(&PASSWORD), BleResult::Done);
    assert_eq!(
        app.op(read(uuids::CRYPTO_SERVICE, uuids::NEW_PUBLIC_KEY)),
        BleResult::Status(0x89)
    );
    app.sim.key = None;
    assert_eq!(
        app.op(read(uuids::CRYPTO_SERVICE, uuids::NEW_PUBLIC_KEY)),
        BleResult::Status(0x81)
    );
}

#[test]
fn the_pump_takes_whatever_key_the_blob_names_as_the_other_side() {
    let mut app = App::connected();
    let offer = offer(&mut app);
    assert_eq!(
        exchange(&mut app, &blob(SERVER, &offer, public(APP), 1)),
        BleResult::Done
    );
    assert_eq!(app.sim.key, Some(derive(APP, app.sim.pump_public())));
}

#[test]
fn a_blob_that_does_not_open_or_does_not_answer_the_challenge_is_0x8a() {
    let mut app = App::connected();
    let offer = offer(&mut app);
    let mut wrong_challenge = offer.clone();
    wrong_challenge[0] ^= 1;
    for bad in [
        blob([0x44; KEY_LEN], &offer, public(SERVER), 1),
        blob(SERVER, &wrong_challenge, public(SERVER), 1),
        blob(SERVER, &offer, public(SERVER), 1)[..115].to_vec(),
    ] {
        assert_eq!(exchange_any(&mut app, &bad), BleResult::Status(0x8A));
    }
    assert_eq!(app.sim.key, Some(KEY));
}

fn exchange_any(app: &mut App, blob: &[u8]) -> BleResult {
    Frames::split(blob)
        .unwrap()
        .into_iter()
        .map(|frame| {
            app.op(write(
                uuids::CRYPTO_SERVICE,
                uuids::WRITE_CHALLENGE_UTC_APP_PUB_KEY,
                &frame,
            ))
        })
        .last()
        .unwrap()
}

#[test]
fn a_counter_not_above_the_stored_one_is_0x8b() {
    let mut app = App::connected();
    app.sim.pairing_counter = 500;
    let offer = offer(&mut app);
    for counter in [499, 500] {
        assert_eq!(
            exchange(&mut app, &blob(SERVER, &offer, public(SERVER), counter)),
            BleResult::Status(0x8B)
        );
    }
    assert_eq!(app.sim.key, Some(KEY));
}

#[test]
fn a_malformed_exchange_frame_is_rejected_by_the_reassembly() {
    let mut app = App::connected();
    assert_eq!(
        app.op(write(
            uuids::CRYPTO_SERVICE,
            uuids::WRITE_CHALLENGE_UTC_APP_PUB_KEY,
            &[Frames::header(2, 7), 1]
        )),
        BleResult::Status(0x8D)
    );
}
