//! The saved video volume (TGB-3050).
//!
//! tdesktop keeps one video volume for the media viewer, Picture-in-Picture and stories in
//! the settings (`Core::Settings::videoVolume`): each player starts with it, and every change
//! sets the mixer's video volume and saves the settings a second later
//! (`OverlayWidget::playbackControlsVolumeChanged`, `saveSettingsDelayed`). Here the mixer's
//! video volume ([`VideoSoundOutput::volume`]) is the live value every player reads and
//! writes, and [`VideoVolumeSync`] mirrors it into `CoreSettings::video_volume`.

use std::time::{Duration, Instant};

use crate::media::VideoSoundOutput;

/// How long a volume change waits before it is saved (`Core::Application::kDefaultSaveDelay`).
pub const SAVE_DELAY: Duration = Duration::from_millis(1_000);

/// Keeps the mixer's video volume and the saved setting in step; call [`Self::sync`] once per
/// frame.
#[derive(Debug, Default)]
pub struct VideoVolumeSync {
    /// The volume last seen on (or written to) the mixer.
    applied: Option<f64>,
    /// A change not saved yet, with the time it was last changed.
    pending: Option<(f64, Instant)>,
}

impl VideoVolumeSync {
    /// Creates a sync that loads the saved volume into the mixer on the first [`Self::sync`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            applied: None,
            pending: None,
        }
    }

    /// First call: the mixer takes the saved volume. Later calls: a volume a player changed
    /// is written to `saved` once it stayed the same for [`SAVE_DELAY`] (a slider drag saves
    /// once); a `saved` value changed elsewhere (settings reset) goes to the mixer. Returns
    /// `true` when `saved` changed.
    pub fn sync(&mut self, output: &VideoSoundOutput, saved: &mut f64, now: Instant) -> bool {
        let Some(applied) = self.applied else {
            let volume = saved.clamp(0.0, 1.0);
            output.set_volume(volume);
            self.applied = Some(output.volume());
            return false;
        };
        let current = output.volume();
        if current != applied {
            self.applied = Some(current);
            self.pending = Some((current, now));
        }
        if let Some((volume, changed_at)) = self.pending {
            if now.saturating_duration_since(changed_at) < SAVE_DELAY {
                return false;
            }
            self.pending = None;
            if *saved != volume {
                *saved = volume;
                return true;
            }
            return false;
        }
        if *saved != current {
            output.set_volume(saved.clamp(0.0, 1.0));
            self.applied = Some(output.volume());
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tg_audio::MixerCore;

    use super::*;

    #[test]
    fn saved_volume_reaches_the_mixer_and_changes_are_saved_after_a_second() {
        let output = VideoSoundOutput::new(Arc::new(Mutex::new(MixerCore::new())), None);
        let mut sync = VideoVolumeSync::new();
        let t0 = Instant::now();
        let mut saved = 0.35;
        assert!(!sync.sync(&output, &mut saved, t0));
        assert!(
            (output.volume() - 0.35).abs() < 1e-9,
            "players start at the saved volume"
        );

        // A slider drag: several changes, saved once, a second after the last one.
        for (i, v) in [0.4, 0.5, 0.6].into_iter().enumerate() {
            output.set_volume(v);
            let at = t0 + Duration::from_millis(100 * (i as u64 + 1));
            assert!(!sync.sync(&output, &mut saved, at));
        }
        assert!(!sync.sync(&output, &mut saved, t0 + Duration::from_millis(1_200)));
        assert!((saved - 0.35).abs() < 1e-9, "not saved before the delay");
        assert!(sync.sync(&output, &mut saved, t0 + Duration::from_millis(1_300)));
        assert!((saved - 0.6).abs() < 1e-9);
        assert!(!sync.sync(&output, &mut saved, t0 + Duration::from_millis(5_000)));

        // Muting saves 0, like tdesktop.
        output.set_volume(0.0);
        let _ = sync.sync(&output, &mut saved, t0 + Duration::from_millis(6_000));
        assert!(sync.sync(&output, &mut saved, t0 + Duration::from_millis(7_000)));
        assert!(saved.abs() < 1e-9);

        // A settings reset elsewhere goes back to the mixer.
        saved = 0.9;
        assert!(!sync.sync(&output, &mut saved, t0 + Duration::from_millis(8_000)));
        assert!((output.volume() - 0.9).abs() < 1e-9);
    }
}
