pub fn effective_trial_koi_status(db_status: &str, run_slot_active: bool) -> &str {
    if run_slot_active {
        "busy"
    } else {
        db_status
    }
}
