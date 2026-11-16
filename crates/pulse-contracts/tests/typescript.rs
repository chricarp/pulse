//! Fails when `apps/web/src/contracts/index.ts` is stale. Regenerate with
//! `PULSE_UPDATE_TS=1 cargo test -p pulse-contracts --test typescript`.

use std::path::PathBuf;

#[test]
fn typescript_bindings_are_current() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/web/src/contracts/index.ts");
    let generated = pulse_contracts::typescript::typescript_bindings();
    if std::env::var_os("PULSE_UPDATE_TS").is_some() {
        std::fs::write(&path, &generated).unwrap();
        return;
    }
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        current == generated,
        "{} is stale. Regenerate with: PULSE_UPDATE_TS=1 cargo test -p pulse-contracts --test typescript",
        path.display()
    );
}
