//! TGB-3000: `BackgroundAudioStream` decodes on its own thread, so the mixer never waits for
//! bytes that are still downloading: it plays silence and keeps the position until the decoder
//! has samples, and a seek drops what was decoded before it.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use tg_audio::{
    AudioMsgId, AudioStream, AudioType, BackgroundAudioStream, MemoryPcmStream, MixerCore,
    PlaybackState,
};

fn tone(secs: usize) -> Vec<f32> {
    (0..48_000 * secs)
        .map(|i| ((i as f32) * 0.05).sin() * 0.4)
        .collect()
}

fn voice_id() -> AudioMsgId {
    AudioMsgId {
        external_play_id: AudioMsgId::create_external_play_id(),
        audio_type: AudioType::Voice,
        duration_ms: 3_000,
        ..AudioMsgId::default()
    }
}

/// A stream whose open waits until the test sends the go-ahead, like a file whose first part
/// is still downloading.
fn gated_stream(secs: usize) -> (BackgroundAudioStream, mpsc::Sender<()>) {
    let (go, wait) = mpsc::channel::<()>();
    let samples = tone(secs);
    let stream = BackgroundAudioStream::spawn(
        Box::new(move || {
            wait.recv_timeout(Duration::from_secs(20))
                .map_err(|e| tg_audio::AudioError::SourceNotFound(e.to_string()))?;
            Ok(Box::new(MemoryPcmStream::new(samples, 48_000, 1)) as Box<dyn AudioStream>)
        }),
        secs as i64 * 1000,
    )
    .expect("spawn");
    (stream, go)
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn the_mixer_plays_silence_and_holds_the_position_until_the_stream_has_data() {
    let (stream, go) = gated_stream(3);
    assert!(stream.waiting_for_data());
    let mut mixer = MixerCore::new();
    let id = voice_id();
    mixer
        .play(id, Box::new(stream), 0, 1.0, false)
        .expect("play");

    let mut buf = vec![0.0_f32; 480 * 2];
    for _ in 0..20 {
        let started = Instant::now();
        let _ = mixer.render_stereo_48k(&mut buf);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "a render never waits for the stream"
        );
        assert!(buf.iter().all(|s| *s == 0.0), "silence while waiting");
    }
    let state = mixer.current_state(AudioType::Voice);
    assert_eq!(state.position, 0, "the position stays while waiting");
    assert_eq!(
        state.state,
        PlaybackState::Playing,
        "the track keeps playing"
    );

    go.send(()).expect("go");
    wait_until("sound", || {
        let _ = mixer.render_stereo_48k(&mut buf);
        buf.iter().any(|s| s.abs() > 0.01)
    });
    wait_until("the position to move", || {
        let _ = mixer.render_stereo_48k(&mut buf);
        mixer.current_state(AudioType::Voice).position > 0
    });
}

#[test]
fn a_seek_drops_the_samples_decoded_before_it_and_the_stream_ends() {
    let (mut stream, go) = gated_stream(2);
    go.send(()).expect("go");
    wait_until("the first chunk", || !stream.waiting_for_data());
    stream.seek_ms(1_500).expect("seek");
    let mut total = 0usize;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "the stream ends");
        if stream.waiting_for_data() {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        match stream.read_chunk().expect("chunk") {
            Some(chunk) => total += chunk.samples.len(),
            None => break,
        }
    }
    assert_eq!(total, 24_000, "only the half second after the seek is read");
}

#[test]
fn an_open_failure_stops_the_track_with_an_error() {
    let stream = BackgroundAudioStream::spawn(
        Box::new(|| Err(tg_audio::AudioError::SourceNotFound("no loader".into()))),
        1_000,
    )
    .expect("spawn");
    let mut mixer = MixerCore::new();
    mixer
        .play(voice_id(), Box::new(stream), 0, 1.0, false)
        .expect("play");
    let mut buf = vec![0.0_f32; 480 * 2];
    wait_until("the error", || {
        let _ = mixer.render_stereo_48k(&mut buf);
        mixer.current_state(AudioType::Voice).state == PlaybackState::StoppedAtError
    });
}
