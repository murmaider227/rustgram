//! CLI example demonstrating playback of `.ogg` (Opus/Vorbis), `.mp3`, `.flac`,
//! `.wav`, or `.aac`/`.m4a` files through `tg-audio`.
//!
//! Usage:
//! `cargo run -p tg-audio --example play -- path/to/file.ogg [--speed 1.5] [--volume 0.8]`

use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tg_audio::{
    AudioDecoder, AudioInput, AudioMsgId, AudioOutputEngine, AudioStream, AudioType, DeviceManager,
    DocumentId, FullMsgId, MediaKind, MixerCore, MsgId, PeerId, clamp_speed,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!(
            "Usage: cargo run -p tg-audio --example play -- <audio-file> [--speed <0.5..2.5>] [--volume <0.0..1.0>]"
        );
        std::process::exit(2);
    }

    let mut path: Option<PathBuf> = None;
    let mut speed = 1.0f64;
    let mut volume = 1.0f64;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--speed" if i + 1 < args.len() => {
                speed = clamp_speed(args[i + 1].parse::<f64>()?);
                i += 2;
            }
            "--volume" if i + 1 < args.len() => {
                volume = args[i + 1].parse::<f64>()?.clamp(0.0, 1.0);
                i += 2;
            }
            arg => {
                path = Some(PathBuf::from(arg));
                i += 1;
            }
        }
    }

    let path = path.ok_or("missing audio file path")?;
    let input = AudioInput::from_path(&path)?;
    let decoder = AudioDecoder::open(input)?;
    let attrs = decoder.attributes().clone();

    println!(
        "Playing: {} ({} Hz, {} ch, {:.2}s, speed {:.2}x, volume {:.0}%)",
        path.display(),
        decoder.sample_rate(),
        decoder.channels(),
        attrs.duration_ms as f64 / 1000.0,
        speed,
        volume * 100.0
    );
    if let Some(title) = &attrs.title {
        println!("  Title: {title}");
    }
    if let Some(performer) = &attrs.performer {
        println!("  Artist: {performer}");
    }

    let devices = DeviceManager::shared();
    if !devices.has_playback_device() {
        eprintln!("No audio playback device available on this system.");
        return Ok(());
    }

    let mixer = Arc::new(Mutex::new(MixerCore::new()));
    let output = AudioOutputEngine::new(devices, Arc::clone(&mixer));

    let id = AudioMsgId::new(
        DocumentId(1),
        FullMsgId::new(PeerId(1), MsgId(1)),
        MediaKind::AudioFile,
        attrs.duration_ms,
        0,
    );

    {
        let mut m = mixer.lock().map_err(|_| "failed to lock mixer")?;
        m.set_song_volume(volume);
        m.play(id, Box::new(decoder), 0, speed, false)?;
    }

    output.ensure_attached()?;

    loop {
        thread::sleep(Duration::from_millis(100));
        let state = mixer
            .lock()
            .map_err(|_| "failed to lock mixer")?
            .current_state(AudioType::Song);
        if state.state.is_stopped() {
            println!("\nPlayback finished ({:?}).", state.state);
            break;
        }
        eprint!(
            "\rPosition: {:.2}s / {:.2}s   ",
            state.position_ms() as f64 / 1000.0,
            state.length_ms() as f64 / 1000.0
        );
    }

    output.detach();
    Ok(())
}
