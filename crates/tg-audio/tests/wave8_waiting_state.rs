//! TGB-3115: the track state says when a playing track waits for its stream
//! (`TrackState::waiting_for_data`, tdesktop's `waitingForData`), so the UI can show its waiting
//! radial. The old mixer played silence for it but never set the flag.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use tg_audio::{
    AudioMsgId, AudioStream, AudioType, BackgroundAudioStream, MemoryPcmStream, MixerCore,
};

fn voice_id() -> AudioMsgId {
    AudioMsgId {
        external_play_id: AudioMsgId::create_external_play_id(),
        audio_type: AudioType::Voice,
        duration_ms: 2_000,
        ..AudioMsgId::default()
    }
}

/// A two-second stream whose open waits for the go-ahead, like a file still downloading.
fn gated_stream() -> (BackgroundAudioStream, mpsc::Sender<()>) {
    let (go, wait) = mpsc::channel::<()>();
    let samples: Vec<f32> = (0..96_000)
        .map(|i| ((i as f32) * 0.05).sin() * 0.4)
        .collect();
    let stream = BackgroundAudioStream::spawn(
        Box::new(move || {
            wait.recv_timeout(Duration::from_secs(20))
                .map_err(|e| tg_audio::AudioError::SourceNotFound(e.to_string()))?;
            Ok(Box::new(MemoryPcmStream::new(samples, 48_000, 1)) as Box<dyn AudioStream>)
        }),
        2_000,
    )
    .expect("spawn");
    (stream, go)
}

#[test]
fn a_playing_track_reports_while_it_waits_for_its_stream() {
    let (stream, go) = gated_stream();
    let mut mixer = MixerCore::new();
    mixer
        .play(voice_id(), Box::new(stream), 0, 1.0, false)
        .expect("play");
    let mut buf = vec![0.0_f32; 480 * 2];
    for _ in 0..5 {
        let _ = mixer.render_stereo_48k(&mut buf);
    }
    assert!(
        mixer.current_state(AudioType::Voice).waiting_for_data,
        "waiting while the stream has no data"
    );

    go.send(()).expect("go");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let _ = mixer.render_stereo_48k(&mut buf);
        if buf.iter().any(|s| s.abs() > 0.01) {
            break;
        }
        assert!(Instant::now() < deadline, "no sound");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        !mixer.current_state(AudioType::Voice).waiting_for_data,
        "not waiting once it plays"
    );
}

#[test]
fn a_paused_track_does_not_report_waiting() {
    let (stream, _go) = gated_stream();
    let mut mixer = MixerCore::new();
    let id = voice_id();
    mixer
        .play(id, Box::new(stream), 0, 1.0, false)
        .expect("play");
    let mut buf = vec![0.0_f32; 480 * 2];
    let _ = mixer.render_stereo_48k(&mut buf);
    assert!(mixer.current_state(AudioType::Voice).waiting_for_data);
    mixer.pause(id, true);
    for _ in 0..200 {
        let _ = mixer.render_stereo_48k(&mut buf);
    }
    assert!(!mixer.current_state(AudioType::Voice).waiting_for_data);
}

/// A pause or stop fades out over the samples it plays; while the stream waits there are none,
/// so the old mixer left the track `Pausing` / `Stopping` forever (and counted it as active).
#[test]
fn a_pause_or_stop_while_waiting_completes() {
    use tg_audio::PlaybackState;

    let mut buf = vec![0.0_f32; 480 * 2];
    let (stream, _go) = gated_stream();
    let mut mixer = MixerCore::new();
    let id = voice_id();
    mixer
        .play(id, Box::new(stream), 0, 1.0, false)
        .expect("play");
    let _ = mixer.render_stereo_48k(&mut buf);
    mixer.pause(id, false);
    for _ in 0..10 {
        let _ = mixer.render_stereo_48k(&mut buf);
    }
    assert_eq!(
        mixer.current_state(AudioType::Voice).state,
        PlaybackState::Paused
    );

    let (stream, _go) = gated_stream();
    let id = voice_id();
    mixer
        .play(id, Box::new(stream), 0, 1.0, false)
        .expect("play");
    let _ = mixer.render_stereo_48k(&mut buf);
    mixer.stop(id);
    for _ in 0..10 {
        let _ = mixer.render_stereo_48k(&mut buf);
    }
    assert!(mixer.current_state(AudioType::Voice).state.is_stopped());
    assert!(!mixer.has_active_audio());
}
