# YpsoPump behaviours to confirm on the bench

What the simulator implements from the spec, Rockit and the firmware verification but that no source settles. Each item names the bench test that settles it. Tick it and update the simulator when done.

| # | Point | Simulator today | Bench test |
|---|---|---|---|
| 1 | ControlNotify payload during a bolus | not implemented | subscribe to ControlNotify during a bolus and capture the frames |
| 2 | Trailer of bolus status and system status: genCRC16 or u16 length | genCRC16 (Rockit) | capture both decrypted replies on V05.02.03 and test the last 2 bytes against genCRC16 and against the length |
| 3 | What resets the counters and bumps the epoch | only a new pairing key; a reboot keeps counting | read the trailer (reboot, counter) after a reboot, then after a pairing |
| 4 | System status length | 18 bytes OPN, 20 encrypted | capture it with and without a temp basal running |
| 5 | History entry count: entries held or log capacity | entries held | read the count, then walk until 0x84 |
| 6 | Record of a bolus stopped remotely vs on the pump | type 2 with the partial amount; 29 for pump or alarm stops | stop a bolus remotely and from the pump, read both records |
| 7 | Extended bolus cancel record | 30/31 with delivered amount and elapsed minutes | BENCH-ABORT-CHECK run B |
| 8 | Temp basal records | type 10 with minutes run on cancel, 32 on a pump stop | BENCH-ABORT-CHECK run A |
| 9 | Bolus status after the end | status 0x04, fields kept, extended fields cleared | read GetBolusStatus after completion and after a stop |
| 10 | OPN delivery rate | 87 ms per hundredth, as encrypted pumps | time a 5 U bolus on an OPN pump |
| 11 | Refusal codes inferred from "0x82 is out of range" | 0x82 for limits, 0x81 for bad CRC or length, 0x87 for a second extended bolus; accepts a stop with nothing running | send each case and record the code |
| 12 | Pump stop and alarm effects | mode value 3 when stopped, therapy aborted, alarms active until acknowledged, zero alarm payloads | stop from the pump menu, trigger a test occlusion, read setting 2 and both logs |
| 13 | Basal delivery model | 3-minute pulses with carry; status basal rate is the profile slot before the temp basal | watch the reservoir; read the status with a temp basal running |
| 14 | Off-step bolus | accepted | send 0.07 U with the step at 0.10 U |
| 15 | Setting writes | any id accepted; profile switch logs nothing | write a non-basal setting and an out-of-range basal rate |
| 16 | Offline pairing details | Rockit's current choices; fixed challenge; no "challenge in progress" 0x88; 0x81 without key at the exchange write; 0x88/0x89 also block sealed operations | one offline pairing: two offer reads, the blob write, sealed reads at 0, 4 and 6 minutes |
| 17 | `key_agree` result | raw X25519 | contract decision in pump-shared |
| 18 | Access before the password outside settings | 0x08 for everything else; one full-access password | probe each service before the password and after each key |
| 19 | Encrypted-class pump with no session | writes answer 0x8A | write a clear SettingID to an unpaired pump |
| 20 | Link security knobs | apply to everything except version reads | read each characteristic on an unbonded link |
| 21 | Persistence across reconnect | history index and counter selection kept; settings selection reset | reconnect and read a value without selecting first |
| 22 | Combined bolus | immediate part counted in the extended delivered field; sequences from 1 per log | run a combined bolus and read its status |
| 23 | Default values | 10-byte supervisor value and synthetic settings are placeholders | read them on the pump |
