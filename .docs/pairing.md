# Pairing: what exists, what does not

This driver cannot pair a pump. It can only drive a pump that something else
paired, by loading a session blob a host has already written to `Slot::Session`.
`CommandKind::Pair` is deliberately absent from `INFO.commands`
(`src/driver.rs:114`): declaring it would promise a flow the engine does not
implement. This note records exactly where the line falls, so the gap is not
rediscovered as a bug and so nobody fills it by guessing.

## What the engine does today

* `Command::Pair` reaches `plan()` and falls through to the catch-all
  `_ => return Err(Refusal::Unsupported)` (`src/engine/commands.rs:349`). No BLE
  op is emitted; `tests/e2e.rs:395-412` asserts both the refusal and the silence.
* The only route to `EngineState::Paired` is `load()`
  (`src/engine/mod.rs:199-209`): load `Slot::Session`, `Action::Unwrap` it,
  `Session::decode` it (`src/engine/store.rs:29`), then resume the counters from
  `Slot::Counters` and the nonce bound from `Slot::NonceState`. A `None` anywhere
  on that path leaves the engine `Unpaired` (`src/engine/mod.rs:173-181`).
* `Command::Unpair` is the inverse and is implemented: disconnect, erase all
  three slots (`src/engine/mod.rs:211-220`).
* The engine's host-call surface is `Ble`, `Store`, `Erase`, `Load` and `Unwrap`
  (`src/engine/io.rs:138-177`). `Action::Wrap`, `Action::KeyAgree` and
  `Action::Random` are never issued, and `Io::complete` would reject
  `Completion::Wrapped` outright because `Expect` has no variant for it
  (`src/engine/io.rs:14-20`, `src/engine/io.rs:52-62`).
* `Characteristic` (`src/protocol/characteristic.rs:7-41`) has no variant for
  `NEW_PUBLIC_KEY` or `WRITE_CHALLENGE_UTC_APP_PUB_KEY`, so the engine's
  `read`/`write` cannot even name the pairing characteristics. Only
  `EXTENDED_READ` of `CRYPTO_SERVICE` is declared in `INFO.gatt`
  (`src/driver.rs:111`).

So the driver consumes a pairing produced elsewhere. It never produces one.

## The session blob

`Slot::Session` holds a wrapped secret; unwrapped it is this record, which
`Session::decode` (`src/engine/store.rs:29-86`) accepts and nothing in this crate
writes. The test harness builds it by hand
(`tests/e2e_support/mod.rs:55-76`, `src/engine/store.rs:181-198`).

| Offset | Len | Field |
|---|---|---|
| 0 | 2 | magic `YB` |
| 2 | 1 | version: 1 or 3 keyed; 4 keyless (OPN, needs the `opn` feature) |
| 3 | 1 | flags: 0 keyed, 1 keyless |
| 4 | 6 | pump BLE address |
| 10 | 4 | serial, `u32` LE; checked against settings 62/63 at connect (`src/engine/link.rs:444-455`) |
| 14 | 32 | session key for `Crypto` (XChaCha20-Poly1305); ignored when keyless |
| 46 | 8 | write counter, `i64` LE, `>= -1` (`-1` = unseeded) |
| 54 | 8 | read counter, `i64` LE, `>= -1` |
| 62 | 4 | counter epoch, `u32` LE |
| 66 | 8 | validity, `i64` LE; must be 0 when keyless, otherwise read and discarded |
| 74 | 1 | validity kind: 0..=2 always, 3 only with version 3; must be 0 when keyless |
| 75 | 16 | nonce prefix; all-zero is refused |
| 91 | 1 | credential count, `<= 8` |
| 92 | … | credentials, each: 1-byte length (1..=8), that many printable bytes (0x21..0x7F), then a 16-byte password |
| end | 4 | CRC-32 LE over everything above |

Only the first byte of each credential code is kept; `authorise()` uses it to
pick the probe that proves the password took (`'O'`/`'S'`/`'L'` read
`GetSystemStatus`, `'P'` reads `AlarmEntryCount`;
`src/engine/link.rs:458-470`). The 16-byte password itself is written to
`AUTHORIZATION_PASSWORD` in clear.

`Slot::Counters` is a 24-byte record (write, read, epoch, fingerprint) merged
forward only for a matching key fingerprint (`src/engine/store.rs:88-117`).
`Slot::NonceState` is the 16-byte prefix plus a `u64` reserved bound
(`src/engine/store.rs:128-147`).

## The exchange the simulator implements

`src/sim/pairing.rs` and `tests/sim/pairing.rs` specify a complete, testable
pairing exchange on the pump side. It is real, not a stub — but it is only the
pump half. Nothing in `src/engine/` drives it.

1. The link must be encrypted and bonded, and the authorization password must
   have been written, before either characteristic answers
   (`src/sim/pairing.rs:31-36`, `tests/sim/pairing.rs:76-88`). There is no
   passkey step in this crate: `PAIRING_PASSKEY` (`src/protocol/uuids.rs:12`) is
   only listed as a cleartext read and is never exercised.
2. Read `CRYPTO_SERVICE`/`NEW_PUBLIC_KEY`: 64 bytes in 4 clear frames, a 32-byte
   challenge then the pump's X25519 public key (`src/sim/pairing.rs:38-45`,
   `tests/sim/pairing.rs:61-73`). The challenge is fixed in the simulator; real
   firmware behaviour is open (`to-confirm.md` item 16).
3. Write 116 bytes in 7 clear frames to `WRITE_CHALLENGE_UTC_APP_PUB_KEY`:
   `ciphertext(76) || tag(16) || nonce(24)`, XChaCha20-Poly1305 with no AAD
   under `hchacha20(X25519(server_secret, pump_public), [0u8; 16])`. The
   plaintext is `challenge(32) || ttl u32 LE || peer_public(32) || counter u64 LE`
   (`src/sim/pairing.rs:47-82`, `tests/sim/pairing.rs:26-37`).
4. The pump accepts only a counter strictly above the stored one (else `0x8B`)
   and a blob that opens and echoes the challenge (else `0x8A`). On success the
   new session key is `hchacha20(X25519(pump_secret, peer_public), [0u8; 16])`,
   the reboot epoch bumps, both counters reset, and a 300 s cooldown blocks
   every sealed operation with `0x88` (`src/sim/pairing.rs:64-81`,
   `tests/sim/pairing.rs:90-140`).

The blob is sealed under the *server's* key, not the app's. That is the
"offline pairing" shape: the party that can mint a pairing is whoever holds the
server private key. `SimDevice::key_agree` models a host that holds it and
returns raw X25519 (`src/sim/link.rs:222-225`); `to-confirm.md` item 17 records
that "raw X25519" is still a contract decision in `pump-shared`.

## What is missing before `CommandKind::Pair` can be declared

Protocol work, not a declaration. In rough order:

1. **Engine host calls.** Plumb `Action::KeyAgree` and `Action::Random` through
   `src/engine/io.rs` (both answer `Completion::Secret`, so `Expect::Secret`
   covers them), and add `Action::Wrap` with a new `Expect::Wrapped` — without
   it a freshly derived session cannot be written back to the `Class::Secret`
   slot at all.
2. **Characteristics.** Variants for `NEW_PUBLIC_KEY` and
   `WRITE_CHALLENGE_UTC_APP_PUB_KEY`, both cleartext
   (`src/protocol/uuids.rs:102-103`), plus their `INFO.gatt` entries.
3. **Which public key goes in the blob.** Step 3 above puts a peer public key in
   the plaintext and the pump derives the new session key against it
   (`tests/sim/pairing.rs:159-168`). Either the app carries its own X25519
   keypair — which this crate cannot do today, `curve25519-dalek` is a `sim`-only
   optional dependency — or it names the server's public key, which `KeyRef`
   does not expose (`pump-shared` `src/engine/crypto.rs:27-36`): `Action::KeyAgree`
   returns the shared point, never the host key's public half. **This needs a
   contract decision in `pump-shared` before any code here.**
4. **Where the access password comes from.** The blob carries the 16-byte
   authorization password(s), and the pump refuses even the offer read without
   one (step 1). A first pairing has no source for it. `Command::Pair` carries
   only `address` and `server_key`.
5. **The counter and TTL.** The simulator takes any strictly increasing `u64`
   and any `u32` TTL. What real firmware requires (a UTC millisecond stamp? a
   bounded TTL?) is unverified — `to-confirm.md` item 16 names the bench run
   that would settle it.
6. **Post-pairing bookkeeping.** After a successful exchange the pump resets its
   counters and starts a 300 s cooldown; the driver would have to write the new
   session, counters and nonce state, and hold off every sealed operation until
   the cooldown ends.

Until 3, 4 and 5 are settled against hardware or a capture, the honest state is
the one in the tree: no `CommandKind::Pair`, and a host that already holds a
session seeds `Slot::Session` itself.

## If a host needs to migrate an existing pairing

Half of that is now supported. `pump_ypso::inspect_session` is a `pub fn`
over the same parser the engine loads through: it validates a blob and returns
a `SessionSummary` — address, serial, version, keyless flag and the credential
*codes* — or a `SessionError` saying which check failed (`NotASession`,
`Truncated`, `BadChecksum`, `UnsupportedVersion`, `MalformedCredentials`,
`MalformedBody`). Neither type carries the session key or a password, and no
variant quotes any part of the blob, so a host can show the reason to a user
and log it. It is pure: no BLE, no state, and it claims nothing about pairing.

Writing the blob stays the host's job, and it is a small one: `Action::Wrap`
the bytes and store them in `Slot::Session`. Both the keystore and the store
belong to the host, so a host can seed a session with no change here at all —
and a freshly created engine then loads it and reports `Paired`.

There is still **no encoder**. `Session` and the test blob builder are
`pub(crate)`/test-only, and building a session is not the host's job: whoever
can mint a pairing holds the server key. If the group ever wants one, the clean
entry point is a `pub fn` that builds the blob from named fields — but a host
that is handed a blob needs only `inspect_session` and the two host calls
above.
