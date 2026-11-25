//! Security review #3, D5: a launch that cannot take or reach the single-instance lock refuses
//! to start a second full instance on the same `tdata`.

use super::ipc::acquire_or_forward_single_instance;
use super::refuse_without_single_instance;
use tg_platform::single_instance::InstanceActivation;

fn unique_instance_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("RustgramSec3D5Test_{}_{nanos}", std::process::id())
}

#[test]
fn sec3_d5_lock_failure_refuses_to_start_unless_quitting() {
    let err = tg_platform::PlatformError::Ipc("pipe name taken".into());
    let refused = refuse_without_single_instance(&err, false);
    assert!(refused.is_err(), "a normal launch must not continue");
    assert!(
        refused
            .err()
            .is_some_and(|e| e.to_string().contains("will not start a second copy"))
    );
    assert!(refuse_without_single_instance(&err, true).is_ok());
}

#[cfg(windows)]
#[test]
fn sec3_d5_pre_created_pipe_name_fails_the_lock_instead_of_disabling_it() {
    let instance = unique_instance_name();
    let pipe_path = format!("\\\\.\\pipe\\RustgramPipe_{instance}");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    // Another process (here: this test) created the pipe name before the app started.
    let squatter = rt
        .block_on(async {
            tokio::net::windows::named_pipe::ServerOptions::new()
                .first_pipe_instance(true)
                .create(&pipe_path)
        })
        .expect("create squatting pipe");

    let activation = InstanceActivation {
        urls: Vec::new(),
        args: Vec::new(),
        show: true,
        quit: false,
    };
    let outcome = acquire_or_forward_single_instance(&instance, &activation);
    let Err(err) = outcome else {
        panic!("a pre-created pipe name must fail the single-instance lock");
    };
    assert!(
        refuse_without_single_instance(&err, false).is_err(),
        "startup must refuse instead of running without the lock"
    );
    drop(squatter);
}
