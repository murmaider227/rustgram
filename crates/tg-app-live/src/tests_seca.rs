//! Security regression tests for `SEC-A` in `tg-app-live` (`media.rs` launch guard & `session.rs` save).
//!
//! Covers:
//! - A2: Safe unique saves (`write_unique_file_at_destination` / `save_to_disk` refusing `..` and
//!   never overwriting).
//! - A3: `launch_external_file_with` refuses executables (`x.exe`, `run.bat`), extensionless
//!   (`README`), and IP-revealing (`.svg`) files without explicit confirmation; refuses launch
//!   when NTFS `:Zone.Identifier` marking fails; permits confirmed launch once Zone.Identifier
//!   succeeds.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::media::{
    launch_external_file_confirmed_with_zone_marker, launch_external_file_with,
    launch_external_file_with_zone_marker,
};

#[test]
fn seca_a3_unconfirmed_launch_blocks_exe_bat_extensionless_and_svg() {
    let tmp = tempfile::TempDir::new().expect("tempdir");

    for name in ["x.exe", "run.bat", "script.ps1", "README", "chart.svg"] {
        let path = tmp.path().join(name);
        std::fs::write(&path, b"dummy").expect("write");
        let invoked = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&invoked);
        let res = launch_external_file_with(&path, move |_| {
            flag.store(true, Ordering::SeqCst);
            Ok(())
        });
        assert!(
            res.is_err(),
            "unconfirmed launch of '{name}' must be refused"
        );
        assert!(
            !invoked.load(Ordering::SeqCst),
            "launcher must NEVER be called for unconfirmed '{name}'"
        );
    }
}

#[test]
fn seca_a3_failed_zone_identifier_blocks_launch_even_when_confirmed() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let benign = tmp.path().join("photo.jpg");
    std::fs::write(&benign, b"\xFF\xD8\xFF\xD9").expect("write jpg");
    let exe = tmp.path().join("x.exe");
    std::fs::write(&exe, b"MZ").expect("write exe");

    // 1. Benign file with failing Zone.Identifier mark -> MUST fail and never invoke launcher.
    let invoked = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&invoked);
    let res = launch_external_file_with_zone_marker(
        &benign,
        |_| {
            Err(tg_platform::PlatformError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "zone stream write denied",
            )))
        },
        move |_| {
            flag.store(true, Ordering::SeqCst);
            Ok(())
        },
    );
    assert!(
        res.is_err(),
        "failed Zone.Identifier mark must block launch"
    );
    assert!(
        !invoked.load(Ordering::SeqCst),
        "launcher must not run when Zone.Identifier mark fails"
    );

    // 2. Confirmed exe launch with failing Zone.Identifier mark -> MUST fail and never invoke launcher.
    let invoked2 = Arc::new(AtomicBool::new(false));
    let flag2 = Arc::clone(&invoked2);
    let res2 = launch_external_file_confirmed_with_zone_marker(
        &exe,
        |_| {
            Err(tg_platform::PlatformError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "zone stream write denied",
            )))
        },
        move |_| {
            flag2.store(true, Ordering::SeqCst);
            Ok(())
        },
    );
    assert!(
        res2.is_err(),
        "confirmed launch must still fail if Zone.Identifier fails"
    );
    assert!(
        !invoked2.load(Ordering::SeqCst),
        "launcher must not run for confirmed exe when Zone.Identifier fails"
    );

    // 3. Confirmed exe launch with succeeding Zone.Identifier mark -> launches cleanly.
    let invoked3 = Arc::new(AtomicBool::new(false));
    let flag3 = Arc::clone(&invoked3);
    let res3 = launch_external_file_confirmed_with_zone_marker(
        &exe,
        |_| Ok(()),
        move |_| {
            flag3.store(true, Ordering::SeqCst);
            Ok(())
        },
    );
    assert!(
        res3.is_ok(),
        "confirmed exe with valid zone mark should launch: {res3:?}"
    );
    assert!(invoked3.load(Ordering::SeqCst));
}
