//! Addresses from a dApp request or an emulation result are shortened on characters.

use super::*;

#[test]
fn emulation_warning_with_a_non_ascii_account_does_not_panic() {
    let warning = EmulationWarning::TransactionAborted {
        tx_hash: String::new(),
        account: "日本語".repeat(7),
    };
    assert_eq!(
        format_emulation_warning(&warning),
        "Transaction aborted on 日本語日本語日本...日本語日本語"
    );
}

#[test]
fn ascii_account_is_shortened_as_before() {
    let warning = EmulationWarning::TransactionAborted {
        tx_hash: String::new(),
        account: "0:83dfd552e63729b472fcbcc8c45ebcc6691702558b68ec7527e1ba403a0f31a8".to_owned(),
    };
    assert_eq!(
        format_emulation_warning(&warning),
        "Transaction aborted on 0:83dfd5...0f31a8"
    );
}
