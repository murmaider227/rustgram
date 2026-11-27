//! TGB-3111: the Default interface scale controls follow tdesktop's `setScale`: a value that
//! changes the evaluated scale waits for "Restart now?", any other new value is saved at once.

use tg_core::{GlobalConfig, SCALE_AUTO};

use super::scale::{ScaleChange, choose_scale, confirm_scale};

fn config(config_scale: i32, screen_scale: i32) -> GlobalConfig {
    let mut global = GlobalConfig {
        config_scale,
        screen_scale,
        ..GlobalConfig::default()
    };
    global.validate_scale();
    global
}

#[test]
fn a_different_evaluated_scale_needs_a_restart_and_is_not_applied() {
    let mut global = config(SCALE_AUTO, 150);
    assert_eq!(
        choose_scale(&mut global, 200),
        ScaleChange::NeedsRestart(200)
    );
    assert_eq!(
        global.config_scale, SCALE_AUTO,
        "applied only after Restart"
    );
    confirm_scale(&mut global, 200);
    assert_eq!(global.config_scale, 200);
}

#[test]
fn switching_auto_off_at_the_screen_scale_saves_without_a_restart() {
    // "Auto" off writes cEvalScale(cConfigScale()), the screen's own 150 %.
    let mut global = config(SCALE_AUTO, 150);
    let off = global.eval_scale(global.config_scale);
    assert_eq!(choose_scale(&mut global, off), ScaleChange::Saved);
    assert_eq!(global.config_scale, 150);
    // And back on: AUTO evaluates to the same 150 %, again no restart.
    assert_eq!(choose_scale(&mut global, SCALE_AUTO), ScaleChange::Saved);
    assert_eq!(global.config_scale, SCALE_AUTO);
    assert_eq!(choose_scale(&mut global, SCALE_AUTO), ScaleChange::None);
}

#[test]
fn auto_on_a_screen_with_another_scale_needs_a_restart() {
    let mut global = config(125, 150);
    assert_eq!(
        choose_scale(&mut global, SCALE_AUTO),
        ScaleChange::NeedsRestart(SCALE_AUTO)
    );
    assert_eq!(global.config_scale, 125);
}
