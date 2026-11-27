//! TGB-2944: harnesses that render share one wgpu device and take turns on it.
//!
//! `HarnessBuilder::wgpu()` gives each harness its own instance and device. On Windows
//! `egui_kittest` picks WARP, the software D3D12 driver, and WARP devices used from parallel
//! test threads make `d3d10warp.dll` read or write freed memory on one of its thread-pool
//! threads: the whole test process dies with `STATUS_ACCESS_VIOLATION`.
//! `common::serialized_wgpu_renderer` puts every harness on one device that lives for the process
//! and runs each GPU call under one lock (see `shared_wgpu_device` in `tests/common/mod.rs`).
//!
//! Both stress tests build 640 harnesses on 16 threads and render each one. They take tens of
//! seconds, so they only run when asked (`--ignored`).

fn render_many_harnesses(serialized: bool) {
    let threads: Vec<_> = (0..16)
        .map(|t| {
            std::thread::spawn(move || {
                for round in 0..40 {
                    let builder =
                        egui_kittest::Harness::builder().with_size(egui::vec2(320.0, 240.0));
                    let builder = if serialized {
                        builder.renderer(crate::common::serialized_wgpu_renderer())
                    } else {
                        builder.wgpu()
                    };
                    let mut harness = builder.build_ui(move |ui| {
                        ui.label(format!("thread {t} round {round}"));
                    });
                    harness.run();
                    harness.render().expect("rendered");
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().expect("render thread");
    }
}

#[test]
#[ignore = "tens of seconds; run with --ignored to check the shared device under stress"]
fn many_harnesses_on_many_threads_render_on_the_shared_device() {
    render_many_harnesses(true);
}

#[test]
#[ignore = "kills the test process on Windows (WARP access violation): shows why it is shared"]
fn one_device_per_harness_crashes_warp_under_parallel_load() {
    render_many_harnesses(false);
}
