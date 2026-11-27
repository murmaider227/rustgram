//! High-level media player: playlists per type (Voice/Round vs Song/Music),
//! repeat/order/shuffle modes, playback speed, seeking, pause-on-call,
//! voice-vs-music coordination, and per-document resume positions.
//!
//! Port of `media/player/media_player_instance.h` and
//! `media/player/media_player_instance.cpp`.

use std::sync::{Arc, Mutex};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::error::{AudioError, AudioResult};
use crate::mixer::{MixerCore, MixerEvent};
use crate::output::AudioOutputEngine;
use crate::source::{
    AudioDataSource, PlaylistContext, PlaylistEntry, PlaylistProvider, ResumePositionStore,
};
use crate::types::{
    AudioMsgId, AudioType, DURATION_UNAVAILABLE, MIN_LENGTH_FOR_SAVE_POSITION_MUSIC_SECS,
    MIN_LENGTH_FOR_SAVE_POSITION_VIDEO_SECS, OrderMode, PlaybackState, RepeatMode, SPED_UP_DEFAULT,
    TIME_UNKNOWN, TrackState, clamp_speed,
};
use tg_data::FullMsgId;

/// Number of recently shuffled items remembered before recycling (`kRememberShuffledOrderItems`).
pub const REMEMBER_SHUFFLED_ORDER_ITEMS: usize = 16;

/// Seeking gesture transition (`Media::Player::Instance::Seeking`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SeekingPhase {
    /// User started dragging the seek bar.
    Start,
    /// User released the seek bar at a target position.
    Finish,
    /// Seek gesture was cancelled.
    Cancel,
}

/// High-level events emitted by [`PlayerInstance`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerEvent {
    /// Track state or position updated.
    Updated(TrackState),
    /// Current track in `audio_type` changed.
    TrackChanged(AudioType),
    /// Auto-advanced from `from` to the next message `to`.
    SwitchToNext {
        /// Track that finished.
        from: AudioMsgId,
        /// Context message ID of the next track.
        to: FullMsgId,
    },
    /// Playback reached the end of the playlist for `audio_type`.
    TracksFinished(AudioType),
    /// Playback stopped for `audio_type`.
    Stopped(AudioType),
    /// Playback started for `audio_type`.
    StartedPlay(AudioType),
    /// Seeking gesture phase changed.
    SeekingChanged {
        /// Audio slot being seeked.
        audio_type: AudioType,
        /// New seeking phase.
        phase: SeekingPhase,
    },
    /// Playlist contents, repeat mode, or order mode changed.
    PlaylistChanged(AudioType),
}

/// Computes and persists the resume playback position for a document.
///
/// Exact port of `SaveLastPlaybackPosition` (`media_player_instance.cpp:127-145`).
pub fn save_last_playback_position(
    audio: &AudioMsgId,
    state: &TrackState,
    store: &dyn ResumePositionStore,
) {
    if audio.document_id.is_zero() {
        return;
    }
    let limit_secs = if audio.audio_type == AudioType::Video {
        MIN_LENGTH_FOR_SAVE_POSITION_VIDEO_SECS
    } else {
        MIN_LENGTH_FOR_SAVE_POSITION_MUSIC_SECS
    };
    let freq = i64::from(state.frequency);
    let saved_ms = if state.position == TIME_UNKNOWN
        || state.length == TIME_UNKNOWN
        || freq <= 0
        || state.state == PlaybackState::PausedAtEnd
        || state.state.is_stopped()
    {
        0
    } else if state.length >= limit_secs * freq {
        (state.position / freq) * 1000
    } else {
        0
    };

    if store.get_position_ms(audio.document_id) != saved_ms {
        store.set_position_ms(audio.document_id, saved_ms);
    }
}

/// Computes the target seek position in milliseconds from normalized `progress` in `[0.0, 1.0]`.
///
/// Exact port of `Instance::seekStreamed` (`media_player_instance.cpp:1322-1323`).
#[must_use]
pub fn compute_seek_position_ms(progress: f64, duration_ms: i64) -> i64 {
    if duration_ms <= 0 || progress.is_nan() {
        return 0;
    }
    (progress.clamp(0.0, 1.0) * (duration_ms as f64)).round() as i64
}

/// Shuffle state machine faithfully porting `Instance::ShuffleData`,
/// `moveInPlaylist` shuffle branch, and `ensureShuffleMove`
/// (`media_player_instance.cpp:95-111, 652-694, 747-787`).
#[derive(Debug, Clone)]
pub struct ShuffleState {
    /// Complete ordered list of playlist indices (newest-to-oldest or full list).
    pub playlist: Vec<usize>,
    /// Indices not yet played in the current shuffle cycle.
    pub non_played: Vec<usize>,
    /// History of played indices allowing Back/Forward navigation.
    pub played: Vec<usize>,
    /// Cursor into `played`.
    pub index_in_played: usize,
}

impl ShuffleState {
    /// Initializes shuffle state for a playlist of `len` entries.
    #[must_use]
    pub fn new(len: usize) -> Self {
        let playlist: Vec<usize> = (0..len).collect();
        Self {
            non_played: playlist.clone(),
            playlist,
            played: Vec::new(),
            index_in_played: 0,
        }
    }

    /// Returns `true` if stepping backward (`previous`) is available.
    #[must_use]
    pub fn previous_available(&self, repeat_all: bool) -> bool {
        if repeat_all && self.playlist.len() > 1 {
            return true;
        }
        self.index_in_played > 0
    }

    /// Returns `true` if stepping forward (`next`) is available.
    #[must_use]
    pub fn next_available(&self, current_idx: usize, repeat_all: bool) -> bool {
        if repeat_all && self.playlist.len() > 1 {
            return true;
        }
        (self.index_in_played + 1 < self.played.len())
            || (self.non_played.len() > 1)
            || (!self.non_played.is_empty() && self.non_played[0] != current_idx)
    }

    /// Advances forward (`delta > 0`) or backward (`delta < 0`) in shuffle mode.
    ///
    /// Exact port of `media_player_instance.cpp:663-694` and `ensureShuffleMove` (`747-787`).
    pub fn step<R: Rng>(
        &mut self,
        current_idx: usize,
        delta: i32,
        repeat_all: bool,
        rng: &mut R,
    ) -> Option<usize> {
        if self.playlist.is_empty() {
            return None;
        }

        if self.index_in_played == self.played.len() {
            self.played.push(current_idx);
            if let Some(pos) = self.non_played.iter().position(|&id| id == current_idx) {
                self.non_played.remove(pos);
            }
        }

        if repeat_all {
            self.ensure_shuffle_move(delta, rng);
        }

        if self.non_played.is_empty()
            && self.index_in_played + 1 == self.played.len()
            && let Some(last) = self.played.pop()
        {
            self.non_played.push(last);
        }

        let shuffle_completed = self.non_played.is_empty()
            || (self.non_played.len() == 1 && self.non_played[0] == current_idx);

        if delta < 0 {
            if self.index_in_played > 0 {
                self.index_in_played -= 1;
                return Some(self.played[self.index_in_played]);
            }
            return None;
        } else if self.index_in_played + 1 < self.played.len() {
            self.index_in_played += 1;
            return Some(self.played[self.index_in_played]);
        }

        if shuffle_completed {
            return None;
        } else if self.index_in_played < self.played.len() {
            self.index_in_played += 1;
        }

        let pick = rng.gen_range(0..self.non_played.len());
        Some(self.non_played[pick])
    }

    fn ensure_shuffle_move<R: Rng>(&mut self, delta: i32, rng: &mut R) {
        if delta < 0 {
            if self.index_in_played > 0 {
                return;
            }
            if self.non_played.len() < 2 && !self.played.is_empty() {
                let free_up = ((self.played.len() / 2).max(
                    self.playlist
                        .len()
                        .saturating_sub(REMEMBER_SHUFFLED_ORDER_ITEMS),
                ))
                .min(self.played.len());
                let split = self.played.len() - free_up;
                self.non_played.extend(self.played.drain(split..));
            }
            if self.non_played.is_empty() {
                return;
            }
            let pick = rng.gen_range(0..self.non_played.len());
            let item = self.non_played.remove(pick);
            self.played.insert(0, item);
            self.index_in_played += 1;
            if self.non_played.is_empty()
                && self.played.len() > 1
                && let Some(last) = self.played.pop()
            {
                self.non_played.push(last);
            }
        } else if self.index_in_played + 1 < self.played.len() {
        } else if self.non_played.len() < 2 && !self.played.is_empty() {
            let free_up = ((self.played.len() / 2).max(
                self.playlist
                    .len()
                    .saturating_sub(REMEMBER_SHUFFLED_ORDER_ITEMS),
            ))
            .min(self.played.len());
            self.non_played.extend(self.played.drain(..free_up));
            self.index_in_played = self.index_in_played.saturating_sub(free_up);
        }
    }
}

/// Standalone playlist navigation helper implementing tdesktop's exact ordering,
/// repeat, reverse, sub-track, TTL-skipping, and shuffle rules.
///
/// Matches `Instance::{trackInItem, moveInItem, moveInPlaylist, previousAvailable, nextAvailable}`
/// in `media_player_instance.cpp:577-846`.
#[derive(Debug, Clone)]
pub struct PlaylistNavigator {
    /// Audio slot type (`Voice` ignores repeat/order settings and always uses `None` + `Default`).
    pub audio_type: AudioType,
    /// Ordered entries (index 0 = oldest, last = newest).
    pub entries: Vec<PlaylistEntry>,
    /// Index of the current entry in `entries`.
    pub current_index: Option<usize>,
    /// Current sub-track index if `entries[current_index]` contains multiple tracks.
    pub current_sub_index: Option<usize>,
    /// Shuffle state when `order == OrderMode::Shuffle`.
    pub shuffle: Option<ShuffleState>,
}

impl PlaylistNavigator {
    /// Creates a new navigator for `audio_type`.
    #[must_use]
    pub fn new(audio_type: AudioType) -> Self {
        Self {
            audio_type,
            entries: Vec::new(),
            current_index: None,
            current_sub_index: None,
            shuffle: None,
        }
    }

    /// Replaces the playlist entries and locates `current`.
    pub fn set_entries(
        &mut self,
        entries: Vec<PlaylistEntry>,
        current: &AudioMsgId,
        order: OrderMode,
    ) {
        self.entries = entries;
        self.locate(current);
        if self.effective_order(order) == OrderMode::Shuffle {
            self.shuffle = Some(ShuffleState::new(self.entries.len()));
        } else {
            self.shuffle = None;
        }
    }

    /// Updates `current_index` and `current_sub_index` to match `audio`.
    pub fn locate(&mut self, audio: &AudioMsgId) {
        self.current_index = None;
        self.current_sub_index = None;
        for (idx, entry) in self.entries.iter().enumerate() {
            if entry.id.same_item(audio) {
                self.current_index = Some(idx);
                if !entry.sub_tracks.is_empty() {
                    self.current_sub_index = entry
                        .sub_tracks
                        .iter()
                        .position(|t| t.document_id == audio.document_id);
                }
                return;
            }
            if let Some(sub_idx) = entry.sub_tracks.iter().position(|t| t.same_item(audio)) {
                self.current_index = Some(idx);
                self.current_sub_index = Some(sub_idx);
                return;
            }
        }
    }

    /// Effective repeat mode (`Voice` is always `RepeatMode::None`, matching lines 1379-1383).
    #[must_use]
    pub const fn effective_repeat(&self, setting: RepeatMode) -> RepeatMode {
        match self.audio_type {
            AudioType::Song => setting,
            _ => RepeatMode::None,
        }
    }

    /// Effective order mode (`Voice` is always `OrderMode::Default`, matching lines 1392-1396).
    #[must_use]
    pub const fn effective_order(&self, setting: OrderMode) -> OrderMode {
        match self.audio_type {
            AudioType::Song => setting,
            _ => OrderMode::Default,
        }
    }

    /// Returns the next or previous track inside the current message's `sub_tracks`, if any.
    #[must_use]
    pub fn track_in_item(&self, delta: i32, order: OrderMode) -> Option<AudioMsgId> {
        let idx = self.current_index?;
        let sub_idx = self.current_sub_index?;
        let entry = self.entries.get(idx)?;
        let step = if self.effective_order(order) == OrderMode::Reverse {
            -delta
        } else {
            delta
        };
        let next = (sub_idx as i32) + step;
        if next >= 0 && (next as usize) < entry.sub_tracks.len() {
            Some(entry.sub_tracks[next as usize])
        } else {
            None
        }
    }

    /// Returns `true` if a previous track exists (`Instance::previousAvailable`).
    #[must_use]
    pub fn previous_available(&self, repeat: RepeatMode, order: OrderMode) -> bool {
        if self.track_in_item(-1, order).is_some() {
            return true;
        }
        let Some(idx) = self.current_index else {
            return false;
        };
        if self.entries.is_empty() {
            return false;
        }
        let repeat = self.effective_repeat(repeat);
        let order = self.effective_order(order);
        if repeat == RepeatMode::All {
            return true;
        }
        if order == OrderMode::Shuffle {
            return self
                .shuffle
                .as_ref()
                .is_some_and(|s| s.previous_available(false));
        }
        if order == OrderMode::Reverse {
            idx + 1 < self.entries.len()
        } else {
            idx > 0
        }
    }

    /// Returns `true` if a next track exists (`Instance::nextAvailable`).
    #[must_use]
    pub fn next_available(&self, repeat: RepeatMode, order: OrderMode) -> bool {
        if self.track_in_item(1, order).is_some() {
            return true;
        }
        let Some(idx) = self.current_index else {
            return false;
        };
        if self.entries.is_empty() {
            return false;
        }
        let repeat = self.effective_repeat(repeat);
        let order = self.effective_order(order);
        if repeat == RepeatMode::All {
            return true;
        }
        if order == OrderMode::Shuffle {
            return self
                .shuffle
                .as_ref()
                .is_some_and(|s| s.next_available(idx, false));
        }
        if order == OrderMode::Reverse {
            idx > 0
        } else {
            idx + 1 < self.entries.len()
        }
    }

    /// Steps by `delta` (`+1` for next, `-1` for previous) and returns the next [`AudioMsgId`].
    ///
    /// Skips self-destruct TTL items (`entry.has_ttl`). When `only_unread_voice` is
    /// `true` and `self.audio_type == AudioType::Voice` during auto-advance, skips
    /// already-read voice messages.
    pub fn step<R: Rng>(
        &mut self,
        delta: i32,
        repeat: RepeatMode,
        order: OrderMode,
        only_unread_voice: bool,
        rng: &mut R,
    ) -> Option<AudioMsgId> {
        if let Some(sub) = self.track_in_item(delta, order) {
            self.locate(&sub);
            return Some(sub);
        }
        let idx = self.current_index?;
        if self.entries.is_empty() {
            return None;
        }

        let repeat = self.effective_repeat(repeat);
        let order = self.effective_order(order);
        let repeat_all = repeat == RepeatMode::All;

        if order == OrderMode::Shuffle {
            let shuffle = self
                .shuffle
                .get_or_insert_with(|| ShuffleState::new(self.entries.len()));
            let next_idx = shuffle.step(idx, delta, repeat_all, rng)?;
            let entry = self.entries.get(next_idx)?;
            if entry.has_ttl {
                return None;
            }
            self.current_index = Some(next_idx);
            self.current_sub_index = if entry.sub_tracks.is_empty() {
                None
            } else {
                Some(0)
            };
            return Some(entry.id);
        }

        let step = if order == OrderMode::Reverse {
            -delta
        } else {
            delta
        };
        let len = self.entries.len() as i32;
        let mut cursor = idx as i32;

        // Walk until we find an eligible non-TTL (and if requested, unread) entry.
        for _ in 0..self.entries.len() {
            let raw_next = cursor + step;
            let candidate = if repeat_all {
                raw_next.rem_euclid(len) as usize
            } else if raw_next < 0 || raw_next >= len {
                return None;
            } else {
                raw_next as usize
            };
            cursor = candidate as i32;
            let entry = &self.entries[candidate];
            if entry.has_ttl {
                continue;
            }
            if only_unread_voice && self.audio_type == AudioType::Voice && !entry.unread {
                continue;
            }
            self.current_index = Some(candidate);
            self.current_sub_index = if entry.sub_tracks.is_empty() {
                None
            } else {
                Some(0)
            };
            return Some(entry.id);
        }
        None
    }
}

struct SlotData {
    current: AudioMsgId,
    seeking: Option<AudioMsgId>,
    context: PlaylistContext,
    navigator: PlaylistNavigator,
    is_playing: bool,
    resume_on_call_end: bool,
    resume_on_voice_end: bool,
}

impl SlotData {
    fn new(audio_type: AudioType) -> Self {
        Self {
            current: AudioMsgId::default(),
            seeking: None,
            context: PlaylistContext::default(),
            navigator: PlaylistNavigator::new(audio_type),
            is_playing: false,
            resume_on_call_end: false,
            resume_on_voice_end: false,
        }
    }
}

/// High-level Telegram media player coordinating Voice/Round and Music/Song playback,
/// playlists, speed, seeking, resume positions, and call ducking/pausing.
///
/// Port of `Media::Player::Instance` (`media_player_instance.cpp`).
pub struct PlayerInstance {
    mixer: Arc<Mutex<MixerCore>>,
    output: Option<Arc<AudioOutputEngine>>,
    source: Arc<dyn AudioDataSource>,
    positions: Arc<dyn ResumePositionStore>,
    playlists: Arc<dyn PlaylistProvider>,
    song: SlotData,
    voice: SlotData,
    repeat_mode: RepeatMode,
    order_mode: OrderMode,
    voice_speed: f64,
    audio_speed: f64,
    disable_autoplay_next: bool,
    voice_autoplay_unread_only: bool,
    in_call: bool,
    rng: StdRng,
    events: Vec<PlayerEvent>,
}

impl PlayerInstance {
    /// Creates a new [`PlayerInstance`].
    ///
    /// Pass `output: Some(engine)` to play through hardware audio via `cpal`,
    /// or `output: None` to drive [`MixerCore::render_stereo_48k`] manually (e.g. in tests).
    #[must_use]
    pub fn new(
        mixer: Arc<Mutex<MixerCore>>,
        output: Option<Arc<AudioOutputEngine>>,
        source: Arc<dyn AudioDataSource>,
        positions: Arc<dyn ResumePositionStore>,
        playlists: Arc<dyn PlaylistProvider>,
    ) -> Self {
        Self {
            mixer,
            output,
            source,
            positions,
            playlists,
            song: SlotData::new(AudioType::Song),
            voice: SlotData::new(AudioType::Voice),
            repeat_mode: RepeatMode::None,
            order_mode: OrderMode::Default,
            voice_speed: 1.0,
            audio_speed: 1.0,
            disable_autoplay_next: false,
            voice_autoplay_unread_only: false,
            in_call: false,
            rng: StdRng::from_entropy(),
            events: Vec::new(),
        }
    }

    /// Seeds the shuffle RNG deterministically (useful in unit tests).
    pub fn seed_rng(&mut self, seed: u64) {
        self.rng = StdRng::seed_from_u64(seed);
    }

    /// Drains all accumulated [`PlayerEvent`]s.
    pub fn take_events(&mut self) -> Vec<PlayerEvent> {
        std::mem::take(&mut self.events)
    }

    /// Returns the currently active player type (`Voice` if a voice/round is active,
    /// otherwise `Song`), matching `Instance::getActiveType` (`media_player_instance.cpp:231`).
    #[must_use]
    pub fn active_type(&self) -> AudioType {
        if self.voice.current.is_valid() {
            let state = self.get_state(AudioType::Voice);
            if !state.state.is_stopped_or_stopping() {
                return AudioType::Voice;
            }
        }
        AudioType::Song
    }

    /// Returns the current [`AudioMsgId`] for `audio_type`.
    #[must_use]
    pub fn current(&self, audio_type: AudioType) -> AudioMsgId {
        match audio_type {
            AudioType::Song => self.song.current,
            AudioType::Voice => self.voice.current,
            _ => AudioMsgId::default(),
        }
    }

    /// Returns the current [`TrackState`] snapshot from the mixer.
    #[must_use]
    pub fn get_state(&self, audio_type: AudioType) -> TrackState {
        self.mixer
            .lock()
            .map(|m| m.current_state(audio_type))
            .unwrap_or_default()
    }

    /// Returns the configured music repeat mode.
    #[must_use]
    pub const fn repeat_mode(&self) -> RepeatMode {
        self.repeat_mode
    }

    /// Sets the music repeat mode (`None`, `One`, `All`).
    pub fn set_repeat_mode(&mut self, mode: RepeatMode) {
        if self.repeat_mode != mode {
            self.repeat_mode = mode;
            self.events
                .push(PlayerEvent::PlaylistChanged(AudioType::Song));
        }
    }

    /// Returns the configured music ordering mode.
    #[must_use]
    pub const fn order_mode(&self) -> OrderMode {
        self.order_mode
    }

    /// Sets the music ordering mode (`Default`, `Reverse`, `Shuffle`).
    pub fn set_order_mode(&mut self, mode: OrderMode) {
        if self.order_mode != mode {
            self.order_mode = mode;
            if mode == OrderMode::Shuffle {
                self.song.navigator.shuffle =
                    Some(ShuffleState::new(self.song.navigator.entries.len()));
            } else {
                self.song.navigator.shuffle = None;
            }
            self.events
                .push(PlayerEvent::PlaylistChanged(AudioType::Song));
        }
    }

    /// Enables or disables automatic advance to the next track at end-of-stream.
    pub fn set_disable_autoplay_next(&mut self, disable: bool) {
        self.disable_autoplay_next = disable;
    }

    /// When `true`, auto-advancing voice/round messages only advances to unread ones.
    pub fn set_voice_autoplay_unread_only(&mut self, unread_only: bool) {
        self.voice_autoplay_unread_only = unread_only;
    }

    /// Returns the effective speed for `audio` (`LookupPlaybackSpeed`, lines 66-76).
    #[must_use]
    pub fn lookup_playback_speed(&self, audio: &AudioMsgId) -> f64 {
        if !audio.changeable_playback_speed {
            return 1.0;
        }
        match audio.audio_type {
            AudioType::Song => self.audio_speed,
            AudioType::Voice => self.voice_speed,
            _ => 1.0,
        }
    }

    /// Sets voice/round playback speed (`0.5..=2.5`, e.g. `1.0`, `1.5`, `1.7`, `2.0`).
    pub fn set_voice_speed(&mut self, speed: f64) {
        self.voice_speed = clamp_speed(speed);
        self.apply_speed_to_active(AudioType::Voice);
    }

    /// Sets music/long-audio playback speed (`0.5..=2.5`).
    pub fn set_audio_speed(&mut self, speed: f64) {
        self.audio_speed = clamp_speed(speed);
        self.apply_speed_to_active(AudioType::Song);
    }

    /// Toggles voice speed between `1.0x` and [`SPED_UP_DEFAULT`] (`1.7x`).
    pub fn toggle_voice_fast_speed(&mut self) -> f64 {
        let next = if (self.voice_speed - 1.0).abs() < 0.05 {
            SPED_UP_DEFAULT
        } else {
            1.0
        };
        self.set_voice_speed(next);
        next
    }

    /// Sets playback speed directly on the active player slot (`Instance::setPlaybackSpeed`).
    pub fn set_playback_speed(&mut self, speed: f64) {
        let active = self.active_type();
        let current = self.current(active);
        if !current.changeable_playback_speed {
            return;
        }
        let speed = clamp_speed(speed);
        match active {
            AudioType::Voice => self.voice_speed = speed,
            AudioType::Song => self.audio_speed = speed,
            _ => return,
        }
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.set_speed(current, speed);
        }
    }

    fn apply_speed_to_active(&self, audio_type: AudioType) {
        let current = self.current(audio_type);
        if !current.is_valid() || !current.changeable_playback_speed {
            return;
        }
        let speed = self.lookup_playback_speed(&current);
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.set_speed(current, speed);
        }
    }

    /// Starts playing a specific `audio` item in `context`.
    ///
    /// Port of `Instance::play(const AudioMsgId &, optional<PlaylistContext>)`.
    pub fn play_item(
        &mut self,
        audio: AudioMsgId,
        context: Option<PlaylistContext>,
    ) -> AudioResult<()> {
        if !audio.is_valid() || !matches!(audio.audio_type, AudioType::Voice | AudioType::Song) {
            return Err(AudioError::InvalidOperation(
                "invalid AudioMsgId for PlayerInstance::play_item".into(),
            ));
        }

        // Save previous track's resume position if switching tracks.
        let prev = self.current(audio.audio_type);
        if prev.is_valid() && prev.document_id != audio.document_id {
            let prev_state = self.get_state(audio.audio_type);
            save_last_playback_position(&prev, &prev_state, self.positions.as_ref());
        }

        if audio.audio_type == AudioType::Voice {
            self.pause_song_for_voice();
        }

        let saved_pos = if !audio.document_id.is_zero() {
            let p = self.positions.get_position_ms(audio.document_id);
            if p > 0 {
                self.positions.set_position_ms(audio.document_id, 0);
            }
            p
        } else {
            0
        };

        let ctx = context.unwrap_or(PlaylistContext {
            peer_id: audio.context_id.peer,
            ..PlaylistContext::default()
        });
        self.start_playback(audio, ctx, saved_pos, false)?;

        if audio.audio_type == AudioType::Voice {
            self.playlists
                .mark_media_read(audio.document_id, audio.context_id);
        }
        self.events.push(PlayerEvent::StartedPlay(audio.audio_type));
        Ok(())
    }

    /// Resumes or replays the current track for `audio_type` (`Instance::play(Type)`).
    pub fn play(&mut self, audio_type: AudioType) -> AudioResult<()> {
        let state = self.get_state(audio_type);
        let current = self.current(audio_type);
        if !current.is_valid() {
            return Ok(());
        }
        if state.state.is_stopped() {
            let ctx = self.slot(audio_type).map(|s| s.context);
            self.play_item(current, ctx)?;
        } else {
            if let Ok(mut mixer) = self.mixer.lock() {
                mixer.resume(current, false);
            }
            if let Some(out) = &self.output {
                let _ = out.ensure_attached();
            }
            let updated = self.get_state(audio_type);
            self.events.push(PlayerEvent::Updated(updated));
        }
        if let Some(slot) = self.slot_mut(audio_type) {
            slot.resume_on_call_end = false;
            slot.resume_on_voice_end = false;
        }
        Ok(())
    }

    /// The hardware output engine, if this player plays through a device.
    #[must_use]
    pub const fn output(&self) -> Option<&Arc<AudioOutputEngine>> {
        self.output.as_ref()
    }

    /// Pauses playback for `audio_type` (`Instance::pause(Type)`).
    pub fn pause(&mut self, audio_type: AudioType) {
        let current = self.current(audio_type);
        if !current.is_valid() {
            return;
        }
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.pause(current, false);
        }
        let updated = self.get_state(audio_type);
        self.events.push(PlayerEvent::Updated(updated));
    }

    /// Toggles play/pause for `audio_type` (`Instance::playPause(Type)`).
    pub fn play_pause(&mut self, audio_type: AudioType) -> AudioResult<()> {
        let state = self.get_state(audio_type);
        if state.state.show_pause_icon() {
            self.pause(audio_type);
            if let Some(slot) = self.slot_mut(audio_type) {
                slot.resume_on_call_end = false;
                slot.resume_on_voice_end = false;
            }
            Ok(())
        } else {
            self.play(audio_type)
        }
    }

    /// Toggles play/pause if `audio` is already current, otherwise plays `audio`.
    pub fn play_pause_item(
        &mut self,
        audio: AudioMsgId,
        context: Option<PlaylistContext>,
    ) -> AudioResult<()> {
        let now = self.current(audio.audio_type);
        if now.same_item(&audio) {
            self.play_pause(audio.audio_type)
        } else {
            self.play_item(audio, context)
        }
    }

    /// Stops playback for `audio_type` (`Instance::stop`).
    pub fn stop(&mut self, audio_type: AudioType, as_finished: bool) {
        let current = self.current(audio_type);
        if current.is_valid() {
            let state = self.get_state(audio_type);
            save_last_playback_position(&current, &state, self.positions.as_ref());
            if let Ok(mut mixer) = self.mixer.lock() {
                mixer.stop_with_state(current, PlaybackState::Stopped);
            }
        }
        if let Some(slot) = self.slot_mut(audio_type) {
            slot.is_playing = false;
            slot.resume_on_call_end = false;
            slot.resume_on_voice_end = false;
        }
        self.events.push(PlayerEvent::Stopped(audio_type));
        if audio_type == AudioType::Voice {
            self.resume_song_after_voice(false);
        }
        if as_finished {
            self.events.push(PlayerEvent::TracksFinished(audio_type));
        }
    }

    /// Stops both Song and Voice playback (`Instance::stopAndClose`).
    pub fn stop_and_close(&mut self) {
        self.stop(AudioType::Song, false);
        self.stop(AudioType::Voice, false);
    }

    /// Returns `true` if `seeking` is active for `audio_type`.
    #[must_use]
    pub fn is_seeking(&self, audio_type: AudioType) -> bool {
        self.slot(audio_type)
            .is_some_and(|s| s.seeking.is_some_and(|id| id == s.current))
    }

    /// Begins a seek gesture on `audio_type` (`Instance::startSeeking`).
    pub fn start_seeking(&mut self, audio_type: AudioType) {
        let current = self.current(audio_type);
        if let Some(slot) = self.slot_mut(audio_type) {
            slot.seeking = Some(current);
        }
        self.pause(audio_type);
        self.events.push(PlayerEvent::SeekingChanged {
            audio_type,
            phase: SeekingPhase::Start,
        });
    }

    /// Updates seek position while keeping playback paused (`Instance::updateSeeking`).
    pub fn update_seeking(&mut self, audio_type: AudioType, progress: f64) -> AudioResult<()> {
        self.seek_progress(audio_type, progress, true)
    }

    /// Completes a seek gesture and resumes playback (`Instance::finishSeeking`).
    pub fn finish_seeking(&mut self, audio_type: AudioType, progress: f64) -> AudioResult<()> {
        self.seek_progress(audio_type, progress, false)?;
        self.cancel_seeking(audio_type);
        self.events.push(PlayerEvent::SeekingChanged {
            audio_type,
            phase: SeekingPhase::Finish,
        });
        Ok(())
    }

    /// Cancels a seek gesture (`Instance::cancelSeeking`).
    pub fn cancel_seeking(&mut self, audio_type: AudioType) {
        if let Some(slot) = self.slot_mut(audio_type) {
            slot.seeking = None;
        }
        let state = self.get_state(audio_type);
        self.events.push(PlayerEvent::Updated(state));
        self.events.push(PlayerEvent::SeekingChanged {
            audio_type,
            phase: SeekingPhase::Cancel,
        });
    }

    /// Seeks directly to an absolute millisecond position on `audio_type`.
    pub fn seek_ms(&mut self, audio_type: AudioType, position_ms: i64) -> AudioResult<()> {
        let current = self.current(audio_type);
        if !current.is_valid() {
            return Ok(());
        }
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.seek(current, position_ms)?;
        }
        let state = self.get_state(audio_type);
        self.events.push(PlayerEvent::Updated(state));
        Ok(())
    }

    /// Returns `true` if a next track is available in the playlist.
    #[must_use]
    pub fn next_available(&self, audio_type: AudioType) -> bool {
        self.slot(audio_type).is_some_and(|s| {
            s.navigator
                .next_available(self.repeat_mode, self.order_mode)
        })
    }

    /// Returns `true` if a previous track is available in the playlist.
    #[must_use]
    pub fn previous_available(&self, audio_type: AudioType) -> bool {
        self.slot(audio_type).is_some_and(|s| {
            s.navigator
                .previous_available(self.repeat_mode, self.order_mode)
        })
    }

    /// Advances to the next track in `audio_type`'s playlist.
    pub fn next(&mut self, audio_type: AudioType) -> AudioResult<bool> {
        self.move_in_playlist(audio_type, 1, false)
    }

    /// Moves to the previous track in `audio_type`'s playlist.
    pub fn previous(&mut self, audio_type: AudioType) -> AudioResult<bool> {
        self.move_in_playlist(audio_type, -1, false)
    }

    /// Notifies the player that a voice/video call started or ended
    /// (`pauseOnCall` / `resumeOnCall` in `media_player_instance.cpp:208-220, 1198-1219`).
    pub fn set_in_call(&mut self, in_call: bool) -> AudioResult<()> {
        if self.in_call == in_call {
            return Ok(());
        }
        self.in_call = in_call;
        if in_call {
            self.pause_on_call(AudioType::Voice);
            self.pause_on_call(AudioType::Song);
        } else {
            self.resume_on_call(AudioType::Voice)?;
            self.resume_on_call(AudioType::Song)?;
        }
        Ok(())
    }

    /// Feeds [`MixerEvent`]s produced by [`MixerCore::render_stereo_48k`] back into
    /// the player to drive auto-advance, repeat, voice-to-music resume, and UI notifications.
    pub fn handle_mixer_events(&mut self, events: &[MixerEvent]) -> AudioResult<()> {
        for ev in events {
            match *ev {
                MixerEvent::PositionUpdated(id) | MixerEvent::StateChanged(id, _) => {
                    self.handle_track_update(id)?;
                }
                MixerEvent::Stopped(id, _) | MixerEvent::Error(id) => {
                    self.handle_track_update(id)?;
                }
                MixerEvent::SoundTrackFinished(_) => {}
            }
        }
        Ok(())
    }

    fn handle_track_update(&mut self, audio: AudioMsgId) -> AudioResult<()> {
        let audio_type = audio.audio_type;
        if !matches!(audio_type, AudioType::Voice | AudioType::Song) {
            return Ok(());
        }
        let current = self.current(audio_type);
        if current != audio {
            return Ok(());
        }
        let state = self.get_state(audio_type);
        self.events.push(PlayerEvent::Updated(state.clone()));

        let was_playing = self.slot(audio_type).is_some_and(|s| s.is_playing);
        let mut finished = false;
        if was_playing && state.state == PlaybackState::StoppedAtEnd {
            let eff_repeat = self
                .slot(audio_type)
                .map(|s| s.navigator.effective_repeat(self.repeat_mode))
                .unwrap_or(RepeatMode::None);
            if eff_repeat == RepeatMode::One {
                let ctx = self.slot(audio_type).map(|s| s.context);
                self.play_item(current, ctx)?;
            } else if self.disable_autoplay_next || !self.move_in_playlist(audio_type, 1, true)? {
                finished = true;
            }
        }

        if let Some(slot) = self.slot_mut(audio_type) {
            slot.is_playing = !state.state.is_stopped();
        }
        if finished {
            self.events.push(PlayerEvent::TracksFinished(audio_type));
            if audio_type == AudioType::Voice {
                self.resume_song_after_voice(false);
            }
        }
        Ok(())
    }

    fn start_playback(
        &mut self,
        audio: AudioMsgId,
        context: PlaylistContext,
        position_ms: i64,
        start_paused: bool,
    ) -> AudioResult<()> {
        let decoder = self.source.open_stream(&audio)?;
        let speed = self.lookup_playback_speed(&audio);

        let track_changed = self.current(audio.audio_type) != audio;
        let entries = self.playlists.playlist_for(context, audio.audio_type);
        let order = self.order_mode;

        if let Some(slot) = self.slot_mut(audio.audio_type) {
            slot.current = audio;
            slot.context = context;
            slot.is_playing = !start_paused;
            slot.navigator.set_entries(entries, &audio, order);
        }

        {
            let mut mixer = self
                .mixer
                .lock()
                .map_err(|_| AudioError::InvalidOperation("mixer lock poisoned".into()))?;
            mixer.play(audio, decoder, position_ms, speed, start_paused)?;
        }

        if let Some(out) = &self.output {
            let _ = out.ensure_attached();
        }

        if track_changed {
            self.events
                .push(PlayerEvent::TrackChanged(audio.audio_type));
            self.events
                .push(PlayerEvent::PlaylistChanged(audio.audio_type));
        }
        let state = self.get_state(audio.audio_type);
        self.events.push(PlayerEvent::Updated(state));
        Ok(())
    }

    fn seek_progress(
        &mut self,
        audio_type: AudioType,
        progress: f64,
        keep_paused: bool,
    ) -> AudioResult<()> {
        let current = self.current(audio_type);
        if !current.is_valid() {
            return Ok(());
        }
        let state = self.get_state(audio_type);
        let duration_ms = known_duration_ms(state.length_ms(), current.duration_ms);
        if duration_ms <= 0 {
            return Ok(());
        }
        let target_ms = compute_seek_position_ms(progress, duration_ms);
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.seek(current, target_ms)?;
            if keep_paused {
                mixer.pause(current, true);
            } else {
                mixer.resume(current, true);
            }
        }
        let updated = self.get_state(audio_type);
        self.events.push(PlayerEvent::Updated(updated));
        Ok(())
    }

    fn move_in_playlist(
        &mut self,
        audio_type: AudioType,
        delta: i32,
        autonext: bool,
    ) -> AudioResult<bool> {
        let repeat = self.repeat_mode;
        let order = self.order_mode;
        let only_unread = autonext && self.voice_autoplay_unread_only;
        let (from, ctx, next_audio) = {
            let Some(slot) = (match audio_type {
                AudioType::Song => Some(&mut self.song),
                AudioType::Voice => Some(&mut self.voice),
                _ => None,
            }) else {
                return Ok(false);
            };
            let from = slot.current;
            let ctx = slot.context;
            let next = slot
                .navigator
                .step(delta, repeat, order, only_unread, &mut self.rng);
            (from, ctx, next)
        };

        let Some(next_id) = next_audio else {
            return Ok(false);
        };
        if autonext {
            self.events.push(PlayerEvent::SwitchToNext {
                from,
                to: next_id.context_id,
            });
        }
        self.play_item(next_id, Some(ctx))?;
        Ok(true)
    }

    fn pause_on_call(&mut self, audio_type: AudioType) {
        let state = self.get_state(audio_type);
        if !state.id.is_valid()
            || state.state.is_stopped()
            || state.state.is_paused()
            || state.state == PlaybackState::Pausing
        {
            return;
        }
        self.pause(audio_type);
        if let Some(slot) = self.slot_mut(audio_type) {
            slot.resume_on_call_end = true;
        }
    }

    fn resume_on_call(&mut self, audio_type: AudioType) -> AudioResult<()> {
        let should = self
            .slot_mut(audio_type)
            .map(|s| {
                let flag = s.resume_on_call_end;
                s.resume_on_call_end = false;
                flag
            })
            .unwrap_or(false);
        if should {
            self.play(audio_type)?;
        }
        Ok(())
    }

    fn pause_song_for_voice(&mut self) {
        let state = self.get_state(AudioType::Song);
        if !state.id.is_valid()
            || state.state.is_stopped()
            || state.state.is_paused()
            || state.state == PlaybackState::Pausing
        {
            return;
        }
        self.pause(AudioType::Song);
        self.song.resume_on_voice_end = true;
    }

    fn resume_song_after_voice(&mut self, force: bool) {
        if !self.song.resume_on_voice_end && !force {
            return;
        }
        self.song.resume_on_voice_end = false;
        if self.in_call {
            return;
        }
        let _ = self.play(AudioType::Song);
    }

    fn slot(&self, audio_type: AudioType) -> Option<&SlotData> {
        match audio_type {
            AudioType::Song => Some(&self.song),
            AudioType::Voice => Some(&self.voice),
            _ => None,
        }
    }

    fn slot_mut(&mut self, audio_type: AudioType) -> Option<&mut SlotData> {
        match audio_type {
            AudioType::Song => Some(&mut self.song),
            AudioType::Voice => Some(&mut self.voice),
            _ => None,
        }
    }
}

fn known_duration_ms(primary: i64, fallback: i64) -> i64 {
    let is_known = |d: i64| d > 0 && d != TIME_UNKNOWN && d != DURATION_UNAVAILABLE;
    if is_known(primary) {
        primary
    } else if is_known(fallback) {
        fallback
    } else {
        0
    }
}
