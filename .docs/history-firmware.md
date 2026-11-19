# History records established from firmware V05.02.03

Records Rockit leaves undecoded, read from the pump firmware's own writers and decoders (static analysis of `flash_v5_02_03`). A history entry's payload is 12 bytes: three little-endian `u32` values `v0`, `v1`, `v2`, filled field by field by the writer.

Amounts: the firmware converts motor steps with `round(steps / 6)` (`0x0804f388`), the same conversion it uses for the bolus and basal status, so 6 steps = 0.01 U and these amounts are hundredths of a unit.

| Type | Writer | Payload | Meaning |
|---|---|---|---|
| 23 daily total | `LOG_WriteDailyTotal` `0x0804a57c`, decoder `0x0804ad38` | `v0` = `u32` | insulin delivered that day, hundredths of U. Written when the date changes (`0x08024162`, on a SystemDate write, then `DATE_SET`), at the midnight rollover and on storage-mode shutdown; the accumulator is reset right after |
| 4 priming finished | `LOG_WritePrimingDone` `0x0804a5ea`, decoder `0x0804adae` | `v0` bits 0-15 `u16`, bits 16-23 `u8`, bits 24-31 `u8`; `v1` bits 0-15 `i16` | bits 0-15 = amount primed, hundredths of U; the other fields are unidentified status values (keep in `raw`) |
| 25 cannula priming finished | `LOG_WriteCannulaPrimingDone` `0x0804a538` | `v0` bits 0-15 `u16`, bits 16-31 `u16`; `v1` bits 0-15 `i16` | bits 0-15 = amount primed into the cannula, hundredths of U; the others unidentified |
| 5 bolus step changed | `LOG_WriteBolusStepChange` `0x0804a510` | `v0` bits 0-15 `u16` | the new bolus step, in the encoding of setting 3 |
| 33 bolus amount cap changed | `LOG_WriteBolusAmountCapChange` `0x0804a3d0` | `v0` bits 0-15 `i16` new, bits 16-31 `i16` previous | written by the generic setting write for setting 112 (`0x0802435c`) |
| 34 basal rate cap changed | `LOG_WriteBasalRateCapChange` `0x0804a38a` | `v0` bits 0-15 `i16` new, bits 16-31 `i16` previous | written for setting 113 |

Not decoded: backup records 20/21/22 and the system (diagnostic) log, whose records (battery level, LiPo state, motor current, temperature, BLE on/off) carry no therapy meaning.

Access rights (`0x08035460`, one byte per access level 0-3, bit 0 read, bit 1 write): setting 2 (pump mode) and settings 116-119 are writable only at level 3; basal slots from setting 14 at levels 2 and 3. Pump mode changes go through `SYS_SetPumpMode` `0x08024232`, reached from the pump menu, the factory test service and a level-3 write of setting 2.
