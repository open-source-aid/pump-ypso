# Rockit's YpsoPump session export / transfer formats

Written 2026-11-23 from a local Rockit checkout. Purpose: let another app import a YpsoPump
pairing session that Rockit (or its key server) produced, without guessing at a byte layout.

**Sources are a working copy, not a public release.** Every path below is relative to
`~/Documents/rockit/rockit` (git remote `https://github.com/Nascence-Biomed/rockit.git`, HEAD
`13253a2`) unless it says otherwise. `phone/` is a submodule at `05690ae4`. The repository is
**not public** (`gh` / web were not needed because the source was on disk; the remote org
`Nascence-Biomed` was not browsed). Line numbers are from that checkout and will drift.

**Secrets.** Nothing in this document contains key material. No real exported session was found
or opened; every structure below was read from source and specification, not from data.

---

## 0. Executive summary

Rockit has **three** distinct session-carrying formats, and they are not interchangeable:

| # | Format | Direction | Shape |
|---|---|---|---|
| A | `ypso-session-export` | **written by Rockit's key server, read by the Rockit phone app** | passphrase-encrypted JSON file |
| B | `SessionBlob` ("YB") | phone → watch, inside a sealed link | binary; **this is what `pump-ypso` already parses** |
| C | `pendora://session?d=…&k=…` | **read by Rockit, written by a third-party app** (`nightscout/ypso-pandora`) | deeplink wrapping a foreign encrypted bundle |

**If the goal is "another app imports a pairing session from Rockit", format A is the answer.**
It is the only one Rockit treats as an interchange format with an outside consumer, it is
specified in a document written for that purpose, and it has two independent implementations
(Rust and Kotlin) deliberately kept in step.

Format B is the one `pump-ypso` `Session::decode` already reads, but Rockit never emits it to
another *app* — it is a watch payload that travels sealed under the watch link key.

Format C is **not Rockit's format**. Rockit only consumes it.

---

## (a) VERIFIED — with sources

### A. `ypso-session-export` — the passphrase-encrypted key export file

Specification: `phone/docs/KEY-EXPORT.md` (163 lines, the normative document; both
implementations cite it and say its constants must not be "tuned" on one side alone).

Implementations:
- Rust reader + writer: `keyserver/crates/ypso-keyclient/src/key_export.rs`
- Kotlin writer: `keyserver/app/src/main/kotlin/app/ypso/keyserve/KeyExport.kt`
- Rust FFI import surface for the phone app:
  `phone/crates/rockit-ffi/src/key_export.rs`

#### Surface

A **file**, moved by hand (the spec's words: "email, cable, storage, whatever"). Not a URL
scheme, not a QR code, not a pasteboard string.

- Suggested filename: `ypso-key-<pumpSerial>-<YYYY-MM-DD>.json`
  (`KeyExport.kt:152-153`).
- Media type as handled by the Rockit phone app: `application/json`, also `text/plain` and
  `application/octet-stream`, and `content:` URIs — see `phone/apps/android/app/src/main/AndroidManifest.xml:175-188`
  (those intent filters are shared with the Pendora import path).
- Size ceiling on read: **65 536 bytes** (`key_export.rs:153`, `MAX_EXPORT_BYTES`). A real
  export is under a kilobyte.

#### Envelope (outer JSON)

`key_export.rs:16-27` and `KEY-EXPORT.md:23-35`; written at `key_export.rs:370-380`
(Rust) and `KeyExport.kt:114-130` (Kotlin).

```json
{
  "format": "ypso-session-export",
  "version": 1,
  "kdf": { "name": "pbkdf2-hmac-sha256", "iterations": 600000, "salt": "<base64, 16 bytes>" },
  "cipher": "aes-256-gcm",
  "nonce": "<base64, 12 bytes>",
  "ciphertext": "<base64: AES-GCM ciphertext with the 16-byte tag appended>",
  "pump_serial": "10155555",
  "exported_at": "2026-09-16T15:40:00Z"
}
```

- **Binary fields are base64, standard alphabet (`+/`), padded on write.** The Rust reader also
  accepts unpadded, deliberately (`key_export.rs:675-680`). It does **not** accept base64url.
- **No canonical JSON.** Key order and whitespace are unspecified; nothing hashes or signs the
  file bytes (`KEY-EXPORT.md:128-131`). The Kotlin writer pretty-prints with indent 2
  (`KeyExport.kt:131`).
- **There is no checksum and no signature.** The GCM tag is the only integrity check, and it
  covers only salt+nonce+ciphertext — see "the header proves nothing" below.

Constants, all from `keyserver/crates/ypso-keyclient/src/key_export.rs`:

| Constant | Value | Line |
|---|---|---|
| `EXPORT_FORMAT` | `"ypso-session-export"` | 106 |
| `EXPORT_VERSION` | `1` | 108 |
| `EXPORT_KDF` | `"pbkdf2-hmac-sha256"` | 110 |
| `EXPORT_CIPHER` | `"aes-256-gcm"` | 112 |
| `RECOMMENDED_ITERATIONS` (writers) | `600_000` | 118 |
| `MIN_ITERATIONS` (readers refuse below) | `100_000` | 130 |
| `MAX_ITERATIONS` (readers refuse above) | `10_000_000` | 139 |
| `MIN_PASSPHRASE_CHARS` (writers only) | `12` | 148 |
| `MAX_EXPORT_BYTES` | `64 * 1024` | 153 |
| salt accepted | 16–64 bytes (v1 writes 16) | 158-159 |
| nonce | **exactly 12 bytes** | 164 |
| tag | 16 bytes, appended to ciphertext | 168 |
| `MAX_CIPHERTEXT_BYTES` | `32 * 1024` | 176 |

#### Key derivation — the interop trap

`key_export.rs:414-421`, `KEY-EXPORT.md:91-105`, `KeyExport.kt:157-180`.

1. Normalise the passphrase to **Unicode NFC**.
2. Take its **UTF-8** bytes (never UTF-16, never a JCE `char[]`).
3. `PBKDF2-HMAC-SHA256(those bytes, salt, iterations)` → **32 bytes**.

Both steps, in that order, on both sides. The spec is explicit that getting this wrong is
*indistinguishable from a wrong passphrase*, so there will never be an error message pointing at
it. The Kotlin side spells PBKDF2 out over `Mac("HmacSHA256")` rather than
`SecretKeyFactory("PBKDF2WithHmacSHA256")` precisely because providers disagree about `char[]`
encoding (`KeyExport.kt:163-172`).

Leading/trailing spaces are part of the passphrase; callers must not trim, case-fold or
pre-normalise (`phone/crates/rockit-ffi/src/key_export.rs:34-41`).

#### Cipher

AES-256-GCM, 12-byte nonce, 128-bit tag, **no AAD** (`key_export.rs:366-368` for encrypt,
`:424-437` for decrypt; `KeyExport.kt:101-107` says explicitly that adding AAD would produce
files the Rust importer cannot open). Tag appended to the ciphertext, which is both platforms'
default.

#### Plaintext — three accepted shapes

`key_export.rs:473-505` (`parse_contents`). All three are accepted *after* the tag verifies:

1. **Bare session object** — a v1 file with no credential tables; this is what the Kotlin writer
   produces (`KeyExport.kt:90`, `session.toJson()`).
2. `{"session": { … }}` — the wrapper a `GET_SESSION` *response* has.
3. `{"session": { … }, "credential_tables": { … }}` — what the Rust writer produces when the
   exporting phone holds tables (`key_export.rs:350-357`).

A top-level `session` key is what distinguishes a wrapper from a bare session. A
`credential_tables` that is present but malformed is a **hard error**, never silently empty
(`key_export.rs:502-504`); `null` reads as absent.

#### The session object

The same object the wire protocol carries. Rust writer `keyserver/crates/ypso-keyclient/src/session.rs:391-411`;
Rust reader struct `SessionWire` at `session.rs:505-528`; Kotlin writer
`keyserver/app/src/main/kotlin/app/ypso/keyserve/Session.kt:70-82` — field names match exactly.

```json
{
  "version": 1,
  "pump_mac": "AA:BB:CC:DD:EE:FF",
  "pump_serial": "10155555",
  "shared_key": "<64 lowercase hex chars = 32 bytes>",
  "write_counter": -1,
  "read_counter": -1,
  "reboot_counter": 0,
  "captured_at": "2026-09-16T15:40:00Z",
  "key_valid_until": "2026-10-14T15:40:00Z",
  "key_valid_until_source": "mylife",
  "source": ""
}
```

- `version` is **optional on read** (`#[serde(default)] Option<u32>`, `session.rs:506-507`). When
  present it must equal `PROTOCOL_VERSION = 1` (`keyserver/crates/ypso-keyclient/src/lib.rs:149`),
  otherwise the session is rejected (`session.rs:540-547`).
- `pump_mac` and `pump_serial` are **required** (no `serde(default)`), as are `write_counter` and
  `read_counter` (`session.rs:508-512`).
- `shared_key` is the 32-byte XChaCha20-Poly1305 session key as lowercase hex
  (`session.rs:392-397`).
- `write_counter` / `read_counter` are `i64`. **`-1` means *unseeded*, not zero**
  (`session.rs:17` `UNSEEDED_WIRE: i64 = -1`; `:48`, `:59-61`). The FFI and the spec both repeat
  that a `-1` read as `0` has every pump write rejected
  (`phone/crates/rockit-ffi/src/key_export.rs:148-152`, `KEY-EXPORT.md:150-156`).
- `reboot_counter` is `u32`, defaulting to 0 when absent.
- `captured_at`, `key_valid_until`: ISO 8601 UTC, format `yyyy-MM-dd'T'HH:mm:ss'Z'`
  (`Session.kt:107-114`). Default empty.
- `key_valid_until_source`: one of `"mylife"`, `"camaps"`, `"assumed"`; anything else is kept
  verbatim as `Unknown(s)` rather than rejected (`session.rs:194-212`).
- `source`: free text, default empty.
- `auth_credentials` is an **optional** field that exists only in the client's *stored* form, not
  on the wire (`session.rs:523-527`, `:422-436`). A key export written today carries the pump
  passwords in `credential_tables` instead, not here
  (`phone/crates/rockit-ffi/src/key_export.rs:182-186`).

#### The credential tables (optional, inside the ciphertext)

`keyserver/crates/ypso-keyclient/src/credential_tables.rs:282-310`, spec at `KEY-EXPORT.md:58-74`.

```json
{
  "version": 1,
  "rotation": [ { "code": "O2", "after": "", "key": "0x<hex>" } ],
  "pumps":    [ { "name": "YpsoPump_<8 digits>", "code": "P1", "password": "<32 hex>" } ]
}
```

Same shape `GET_CREDENTIAL_TABLES` sends, parsed by the same code so a file cannot disagree with
a fetch. These are secret and must never appear in the unauthenticated header.

#### Validation a consumer MUST perform

From `key_export.rs:545-648` (`Envelope::parse`) and `:250-285` (`decrypt_key_export`):

1. Refuse a file over 64 KiB **before** handing anything to a JSON parser; refuse an empty file.
2. `format` must be exactly `"ypso-session-export"`; otherwise "not a pump key export".
3. `version` must be the **number** `1`. `"1"` as a string is a bad version, not bad JSON — the
   fields are read as `Value` specifically so these two cases report differently
   (`key_export.rs:563-573`).
4. `cipher` must be exactly `"aes-256-gcm"`; `kdf.name` exactly `"pbkdf2-hmac-sha256"`.
5. `kdf.iterations` must be a number, checked as `u64` **before** narrowing to `u32` (so 2^32 is
   refused as too many rather than wrapping), then within `100_000..=10_000_000`.
6. Base64 field lengths are bounded against the *encoded* length first, then checked exactly
   after decoding (`key_export.rs:659-693`). salt 16–64, nonce exactly 12, ciphertext
   `17..=32768`.
7. `pump_serial` is **required and non-empty**.
8. `exported_at` is optional (absence is tolerated; it only drives an "this export is old"
   warning).
9. **Derive the key unconditionally, before looking at the tag.** A wrong passphrase must cost
   exactly what a tampered ciphertext costs.
10. **After the tag verifies, compare the outer `pump_serial` against the inner
    `session.pump_serial` and reject a mismatch** (`key_export.rs:269-282`). This is what
    replaces AAD. Without it, a relabelled file loads a key for the wrong pump.

#### Rules the format imposes on a consumer

From `KEY-EXPORT.md:134-147` and the module header:

- A wrong passphrase and a damaged file must be reported identically — *"that passphrase did not
  work, or the file is damaged"*. Do not try to distinguish them; GCM cannot.
- No passphrase hint, no verification blob, no "check this passphrase" entry point. Reading the
  two header labels (`peek_key_export`, `key_export.rs:207`) is explicitly *not* such a
  thing and must never become one.
- Never write the plaintext anywhere, including a temp file on the way to encrypting it.
- Importing must not silently replace an existing session for that pump.
- Warn (do not block) when `exported_at` is more than 24 hours old.
- Minimum passphrase length 12 is enforced **on write only**, never on read
  (`key_export.rs:328-336`) — enforcing it on read would both refuse legitimate old files and
  report something about the passphrase typed.
- Generated passphrases: 4 hyphen-separated groups of 5 characters from the pairing-code alphabet
  (no `I`, `O`, `0`, `1`), hyphens included, ≈100 bits (`KEY-EXPORT.md:120-124`). The Kotlin
  doc comment at `KeyExport.kt:139-147` says "20 characters" — same thing, 4×5.

#### Versioning, and how to reject an incompatible version

Envelope-level: the single integer `version`, currently `1`. A reader accepts exactly the
versions it knows and refuses the rest by name — `"version is {other}, and this build reads
version 1"` (`key_export.rs:574-581`). The iteration count travels **inside** the file precisely
so it can be raised, or the KDF swapped, in a future version without breaking old exports
(`KEY-EXPORT.md:112-115`).

Plaintext-level: the optional `credential_tables` was added **without** bumping the envelope
version, because it is additive and a session-only v1 file still imports unchanged
(`KEY-EXPORT.md:73-78`). So a consumer must not assume `version: 1` implies "no tables".

The inner session object carries its own `version` field, checked against `PROTOCOL_VERSION = 1`.

#### Who writes one

- Kotlin, in the **key server** app, **debug builds only** — `check(BuildConfig.DEBUG)` at
  `KeyExport.kt:82`, and the spec says so at `KEY-EXPORT.md:145-147`: "a route that puts a live
  key in a file should be a reviewed decision, not a button that shipped by accident". The Kotlin
  writer emits the **bare session** plaintext (no credential tables).
- Rust, `encrypt_key_export` (`key_export.rs:283`, `:299-310`) — **deliberately not exposed over the
  FFI** (same doc comment), so the Rockit *phone* app cannot write one. It exists "so the format
  has a writer to test the reader against". It can emit tables.

#### Who reads one

- `phone/crates/rockit-ffi/src/key_export.rs`: `peek_key_export` (no passphrase, line 121) then
  `import_key_export` (line 155). Exposed to both apps over UniFFI.
- Android UI constant `KEY_EXPORT_FORMAT` at
  `phone/apps/android/app/src/main/kotlin/app/rockit/aid/pump/ypso/driver/KeyFileImport.kt:144`.
- `tools/pump-provision/src/main.rs:149,167` (`pump-provision export <FILE>`), a dev-only CLI
  that decrypts a file and pushes the resulting session to a Pebble watch.

---

### B. `SessionBlob` — the binary "YB" blob

Specification: `drivers/ypso/docs/SESSION-BLOB.md` (172 lines).
Definition: `drivers/ypso/crates/ypso-embedded/src/blob.rs` (`MAGIC` at line 21) — the doc says
the encoder, decoder and their tests *are* the definition.

**This is byte-for-byte the format `pump-ypso` already parses.** I compared
`drivers/ypso/crates/pump-ypso-sansio/src/engine/store.rs:30` (Rockit's copy) against
`~/Documents/open-source-aid/pump-ypso/src/engine/store.rs:29-85`: same `HEADER_LEN = 92`,
same offsets, same strictness, same CRC-32/ISO-HDLC little-endian trailer. `SESSION-BLOB.md`'s
table matches `pump-ypso`'s `Session::decode` field for field, and adds the meanings
`pump-ypso` does not record:

- offset 2 `version`: `1`, `3` (validity source may be CamAPS), `4` (keyless, OPN).
- offset 3 `flags`: `0` in v1/v3, exactly `0x01` in v4.
- offset 74 `validity source`: `0` unknown, `1` mylife, `2` assumed (captured + 28 d), `3` CamAPS
  (v3 only).
- offset 75 nonce prefix: 16 bytes from the phone's CSPRNG, **fresh for every blob**; an all-zero
  prefix is refused.
- credentials: `N` in `0..=8`, each `{ len u8 (1..=8), code (printable ASCII, 0x21..0x7E),
  password 16 bytes }`.
- size 96 bytes + 17–25 per credential; 296 max.
- In v4 the 32-byte "session key" field is **not a pump key** — it is a link root the phone draws
  per binding and which never reaches the pump (`SESSION-BLOB.md:47-57`).

Versioning: a decoder reads exactly the versions it knows and refuses the rest with
`BlobError::Version`, so an older consumer says "update" rather than misreading
(`SESSION-BLOB.md:162-172`). Version 2 is unused on purpose.

**This is not an interchange format between apps.** It is what a phone sends a watch, and it
travels sealed under the watch link key (`drivers/ypso/crates/ypso-watchlink/`,
`phone/crates/rockit-ffi/src/watch_link.rs`). Rockit exposes no file, URL or QR that hands a
raw YB blob to another application. The only tool that emits raw blob bytes is
`pump-provision synthetic --emit-fake-blob`, which by its own documentation produces a **fake**
session for emulator tests (`tools/pump-provision/src/main.rs:84-86`).

---

### C. `pendora://session` — a format Rockit IMPORTS, and does not produce

Recorded here so nobody mistakes it for Rockit's own.

`pendora://session?d=<base64url(bundle)>&k=<base64url(password)>`

- Parser: `phone/crates/rockit-ffi/src/ypso_key_exchange.rs:350-379` (also mirrored in
  `phone/crates/ypso-ffi/src/lib.rs:393-410`).
- Decision record: `docs/decisions/ypso-offline-pair.md` §8b-10 (line 579) and §8b-11 (line 605).
- The producer is the **third-party** app `https://github.com/nightscout/ypso-pandora`
  (`ypso-offline-pair.md:605-622`), whose `App.tsx:123-133` builds the link with both parts
  base64url **unpadded** and a fresh random password per share.
- Rockit registers the scheme to *receive* it: Android `VIEW` filter
  (`AndroidManifest.xml:191-196`), iOS `CFBundleURLTypes` (`phone/apps/ios/Support/Info.plist:114-124`).
- Rockit's parser accepts `d`/`k` base64url padded or unpadded, and `=` written as `%3D`
  (`ypso_key_exchange.rs:310-320`). Bounds: whole link ≤ 256 KiB, `d` ≤ 192 KiB decoded, `k`
  ≤ 1 KiB (`:297-301`). A duplicated `d` or `k` is refused.
- `d` decodes to a **different** encrypted bundle: `version: 1`, `kdf.name: "argon2id"` with
  numeric `t`/`m`(KiB)/`p`, `saltHex`, `nonceHex` (24 bytes, XChaCha20), `ciphertextHex`
  (ciphertext ‖ 16-byte tag); hex, not base64. Plaintext is JSON with
  `vault.sessions[<pumpSerial>]` and a `serverKey { privateKeyHex, slot }`
  (`ypso_key_exchange.rs:62-110`, `:161-228`, `:260-290`).
- Rockit takes **only** the 32-byte X25519 server private key and its slot from it; the session
  key, pump public key, counters and phone identity in the bundle are read past and dropped
  (`ypso_key_exchange.rs:73-77`).
- The whole link is secret because `k` carries the password; Rockit never logs, stores or echoes
  it, and every malformed/undecryptable link gives one fixed message
  (`ypso_key_exchange.rs:303-307`).

**Rockit contains no code that builds a `pendora://` link outside its own unit tests** — I
grepped the whole checkout; the only constructions are in
`ypso_key_exchange.rs:1567-1744` (`#[cfg(test)]`) and in doc comments.

---

### D. `ypsokeys://pair` — a live handshake, not a blob

`ypsokeys://pair?h=<host>&p=<port>&fp=<spki-fingerprint>&c=<one-time-code>&v=1[&bt=<mac>&psm=<psm>|&ble=scan]`

Parser `phone/crates/rockit-ffi/src/pairing_uri.rs:1-30,74-75`; also
`keyserver/crates/ypso-keyclient/src/qr.rs:7` and `keyserver/docs/transport-ble.md:60`. Rendered
as a QR by `phone/apps/ios/Sources/PumpYpso/Views/DistributeWizardView.swift:162,282-310` and
`phone/apps/android/.../pump/ypso/ui/QrImage.kt`.

**This carries no session.** It is an invitation to a short-lived pinned-TLS 1.3 channel over
network or BLE L2CAP, with a 120-second one-time code; the session itself is then fetched over
that channel with the key server protocol (`keyserver/docs/protocol.md`). A consumer wanting it
must implement that whole protocol, not parse a payload.

---

## (b) INFERRED — and how confident

- **Format A is the right target for an importing app.** *High confidence.* It is the only
  format with a written interoperability spec, two implementations held in step, explicit
  version negotiation and explicit rules for a foreign consumer. Nothing else in the tree is
  presented as an app-to-app interchange format.
- **An importer that can parse Rockit's exported session can be fed by the Kotlin writer only,
  today.** *High confidence*, from `check(BuildConfig.DEBUG)` at `KeyExport.kt:79` and
  "Not exposed over the FFI" at `key_export.rs:283`. In practice a user with a *release*
  Rockit build has no button that produces format A. **Verify with Tim Gunn whether that is still
  true in whatever Rockit ships.**
- **The Kotlin writer never emits `credential_tables`.** *High confidence* from
  `KeyExport.kt:90` calling `session.toJson()` directly. So a file produced by the shipping
  writer gives a key and no pump passwords. A reader must handle both and must not treat
  "no tables" as an error.
- **Nothing in format A is signed or attributed.** *High confidence.* The spec says so twice. An
  importer cannot establish that a file came from Rockit at all; it establishes only that
  whoever made it knew the passphrase and that the inner serial matches the outer label.
- **A format-A session converts cleanly into a format-B blob.** *Medium-high confidence.* That
  is exactly what `tools/pump-provision/src/main.rs:24-37` does — decrypt the export, build the
  credential ladder, encode a `SessionBlob` with a fresh CSPRNG nonce prefix. The fields line up
  (`pump_mac`→MAC, `pump_serial`→u32 serial, `shared_key`→key, counters, `key_valid_until`→i64
  seconds, source→the 0–3 enum). I did **not** read `blob.rs`'s encoder line by line, and the
  ISO-8601-string→i64-seconds and `ValidUntilSource`→byte mappings were not traced to code.
  Anyone writing that conversion should read `blob.rs` first.
- **`exported_at`'s exact grammar.** The Kotlin writer emits `yyyy-MM-dd'T'HH:mm:ss'Z'`
  (`Session.kt:107-114`). The Rust reader stores it as an opaque string and never parses it
  (`key_export.rs:186-197`). *Medium confidence* that a consumer should parse leniently and treat
  a failure to parse as "no warning available", never as a rejection.

---

## (c) UNKNOWN — and who could answer

1. **Whether any shipping Rockit build exports format A at all.** Both writers are gated. If the
   answer is "no", then the practical interop story is that Rockit *imports* and some other tool
   must *write*. — **Tim Gunn / the Rockit repo.**
2. **Whether `version: 2` of the envelope exists or is planned**, and whether the KDF would move
   to Argon2id (the spec names Argon2id as "better" and rejected only for Android platform
   reasons; the Pendora path already uses it). — **Tim Gunn.**
3. **The upstream `ypso-rs` repository.** Both `KEY-EXPORT.md` and `KeyExport.kt:16` call the
   spec "ypso-rs `docs/KEY-EXPORT.md`". The copy on disk is `phone/docs/KEY-EXPORT.md`; whether
   `ypso-rs` is public, and whether its copy is newer, was not determined.
4. **`blob.rs`'s encoder details** for a format-A → format-B conversion (date and
   validity-source encoding in particular). Readable in
   `drivers/ypso/crates/ypso-embedded/src/blob.rs`; not read for this note.
5. **Whether the key server's protocol-level `GET_SESSION` would be a better integration point**
   than a file. `keyserver/docs/protocol.md` exists in the checkout and was not read beyond
   locating the session object.
6. **The public/legal status of all of this.** The Rockit remote is `Nascence-Biomed/rockit` and
   was not accessed. `pump-ypso`'s `NOTICE.md` names a derivation from `rockit-driver-ypso` —
   none of the paths in this note match that naming, so the relationship between the checkout
   read here and the crates `NOTICE.md` names is itself unestablished.

---

## Appendix — minimal reader checklist for format A

1. Read at most 65 536 bytes. Reject empty.
2. Parse JSON. Require `format == "ypso-session-export"`, `version == 1` (number),
   `cipher == "aes-256-gcm"`, `kdf.name == "pbkdf2-hmac-sha256"`.
3. `kdf.iterations` ∈ `[100000, 10000000]`, validated as u64 before narrowing.
4. base64-decode (standard alphabet; accept unpadded): `kdf.salt` 16–64 B, `nonce` **12 B**,
   `ciphertext` 17–32768 B.
5. Require non-empty `pump_serial`. Keep `exported_at` as an opaque string.
6. passphrase → NFC → UTF-8 → PBKDF2-HMAC-SHA256(salt, iterations) → 32 B. Always.
7. AES-256-GCM open, 128-bit tag at the end of the ciphertext, **no AAD**. One error for failure.
8. Parse plaintext JSON; unwrap `session` if a top-level `session` key exists; take
   `credential_tables` if present and non-null (strict parse).
9. Require `pump_mac`, `pump_serial`, `shared_key` (64 hex), `write_counter`, `read_counter`.
   `-1` on a counter means **unseeded**.
10. **Reject if outer `pump_serial` != inner `session.pump_serial`.**
11. Zeroize the derived key, the plaintext and the passphrase.
