//! `wasm-pack` copies the crate's own `LICENSE` into the package it builds;
//! that file must be the emulator's, notices included.

#[test]
fn license_is_the_emulators() {
    assert_eq!(
        include_str!("../LICENSE"),
        include_str!("../../../../LICENSE")
    );
}
