#![cfg(feature = "sim")]

mod e2e_support;

use e2e_support::scenarios::*;
use e2e_support::*;
use pump_testkit::Fault;
use pump_ypso::sim::YpsoSim;

fn pump() -> YpsoSim {
    sim()
}

#[test]
#[ignore]
fn connects_and_reads_status_() {
    connects_and_reads_status(pump());
}

#[test]
#[ignore]
fn reads_the_clock_() {
    reads_the_clock(pump());
}

#[test]
#[ignore]
fn syncs_the_clock_() {
    syncs_the_clock(pump());
}

#[test]
#[ignore]
fn reads_and_writes_the_basal_profile_() {
    reads_and_writes_the_basal_profile(pump());
}

#[test]
#[ignore]
fn sets_and_cancels_a_temp_basal_() {
    sets_and_cancels_a_temp_basal(pump());
}

#[test]
fn refuses_what_the_pump_cannot_do_() {
    refuses_what_the_pump_cannot_do(pump());
}

#[test]
fn unpairs_() {
    unpairs(pump());
}

#[test]
#[ignore]
fn survives_a_timeout_at_every_step() {
    survives_a_fault_at_every_step(pump, Fault::BleTimeout);
}

#[test]
#[ignore]
fn survives_a_disconnect_at_every_step() {
    survives_a_fault_at_every_step(pump, Fault::BleDisconnected);
}

#[test]
#[ignore]
fn survives_a_host_failure_at_every_step() {
    survives_a_fault_at_every_step(pump, Fault::HostFails);
}

#[test]
#[ignore]
fn survives_a_counter_error_at_every_step() {
    survives_a_fault_at_every_step(pump, Fault::BleStatus(0x8B));
}

#[test]
fn a_wrong_key_is_key_rejected_() {
    a_wrong_key_is_key_rejected(pump());
}

#[test]
#[ignore]
fn jumps_a_counter_moved_by_another_app_() {
    jumps_a_counter_moved_by_another_app(pump());
}

#[test]
#[ignore]
fn gives_up_after_the_largest_jump_() {
    gives_up_after_the_largest_jump(pump());
}

#[test]
#[ignore]
fn follows_a_pump_reboot_() {
    follows_a_pump_reboot(pump());
}

#[test]
#[ignore]
fn learns_the_epoch_before_jumping_() {
    learns_the_epoch_before_jumping(pump());
}

#[test]
#[ignore]
fn restarts_from_stored_slots_() {
    restarts_from_stored_slots(pump());
}

#[test]
#[ignore]
fn restores_old_storage_() {
    restores_old_storage(pump());
}

#[test]
#[ignore]
fn ignores_notifications_() {
    ignores_notifications(pump());
}

#[cfg(feature = "opn")]
#[test]
#[ignore]
fn drives_an_opn_pump_in_clear_() {
    drives_an_opn_pump_in_clear(opn());
}

#[cfg(feature = "opn")]
#[test]
fn never_drives_an_encrypted_pump_in_clear_() {
    never_drives_an_encrypted_pump_in_clear(pump());
}

#[test]
#[ignore]
fn boluses_() {
    boluses(pump());
}

#[test]
#[ignore]
fn stop_preempts_a_bolus_() {
    stop_preempts_a_bolus(pump());
}

#[test]
#[ignore]
fn a_timed_out_bolus_is_unknown_and_never_resent_() {
    a_timed_out_bolus_is_unknown_and_never_resent(pump());
}

#[test]
#[ignore]
fn extended_bolus_() {
    extended_bolus(pump());
}

#[test]
#[ignore]
fn reads_history_() {
    reads_history(pump());
}

#[test]
#[ignore]
fn every_command_survives_timeouts() {
    sweeps_every_command(pump, Fault::BleTimeout);
}

#[test]
#[ignore]
fn every_command_survives_disconnects() {
    sweeps_every_command(pump, Fault::BleDisconnected);
}

#[test]
#[ignore]
fn every_command_survives_counter_errors() {
    sweeps_every_command(pump, Fault::BleStatus(0x8B));
}

#[test]
#[ignore]
fn every_command_survives_refusals() {
    sweeps_every_command(pump, Fault::BleStatus(0x87));
}

#[test]
#[ignore]
fn every_command_survives_host_failures() {
    sweeps_every_command(pump, Fault::HostFails);
}
