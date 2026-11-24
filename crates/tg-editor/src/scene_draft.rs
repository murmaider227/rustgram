//! Versioned JSON and binary serialization of editor scene drafts, crop/enhance
//! parameters, and multi-clip video timeline drafts.
//!
//! Port of `Telegram/SourceFiles/editor/scene/scene_draft.{h,cpp}` extended with
//! versioned JSON/binary envelopes for full photo/video editor drafts.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::EditorError;
use crate::common::{
    CropMode, CropType, PhotoModifications, RoundedCornersLevel, ShapeType, TextAlignment,
    TextStyle, TextTypeface,
};
use crate::geometry::{Color, PointF, Rect};
use crate::scene::{ItemPlacement, ItemStatus, PaintScene, SceneItemKind, StickerRef};
use crate::scene_items::{AudioTrackClip, LinkPillStyle, LocationData, WeatherData};
use crate::video::clip::MultiClipTimeline;
use crate::video::timeline::OverlayVisibilityTrack;

/// Current supported schema version for [`SceneDraft`].
pub const SCENE_DRAFT_VERSION: u32 = 1;

/// Four-byte magic prefix for binary [`SceneDraft`] blobs (`"TGED"`).
pub const SCENE_DRAFT_BINARY_MAGIC: [u8; 4] = *b"TGED";

/// Maximum binary draft payload size (32 MiB) to guard against malformed inputs.
pub const MAX_DRAFT_BINARY_BYTES: usize = 32 * 1024 * 1024;

const BINARY_HEADER_LEN: usize = 16;

/// Non-destructive crop, orientation, and auto-enhance parameters persisted in a [`SceneDraft`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CropEnhanceParams {
    /// Clockwise rotation in 90-degree steps (`0`, `90`, `180`, `270`).
    #[serde(default)]
    pub angle: i32,
    /// Horizontal flip toggle.
    #[serde(default)]
    pub flipped: bool,
    /// Optional crop rectangle in source pixel coordinates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<Rect>,
    /// Crop overlay/mask shape.
    #[serde(default)]
    pub crop_type: CropType,
    /// Hint vs baked alpha mask mode.
    #[serde(default)]
    pub crop_mode: CropMode,
    /// Corner rounding preset for `CropType::RoundedRect`.
    #[serde(default)]
    pub corners_level: RoundedCornersLevel,
    /// Optional story canvas media placement rect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas_media_rect: Option<Rect>,
    /// Auto-enhance strength (`0..=100`).
    #[serde(default)]
    pub enhance: i32,
}

impl Default for CropEnhanceParams {
    fn default() -> Self {
        Self {
            angle: 0,
            flipped: false,
            crop: None,
            crop_type: CropType::Rect,
            crop_mode: CropMode::Hint,
            corners_level: RoundedCornersLevel::Large,
            canvas_media_rect: None,
            enhance: 0,
        }
    }
}

impl From<&PhotoModifications> for CropEnhanceParams {
    fn from(mods: &PhotoModifications) -> Self {
        Self {
            angle: mods.angle,
            flipped: mods.flipped,
            crop: mods.crop,
            crop_type: mods.crop_type,
            crop_mode: mods.crop_mode,
            corners_level: mods.corners_level,
            canvas_media_rect: mods.canvas_media_rect,
            enhance: mods.enhance,
        }
    }
}

impl CropEnhanceParams {
    /// Converts these crop/enhance parameters and an optional `paint` scene into [`PhotoModifications`].
    #[must_use]
    pub fn to_photo_modifications(&self, paint: Option<PaintScene>) -> PhotoModifications {
        PhotoModifications {
            angle: self.angle,
            flipped: self.flipped,
            crop: self.crop,
            crop_type: self.crop_type,
            crop_mode: self.crop_mode,
            corners_level: self.corners_level,
            paint,
            canvas_media_rect: self.canvas_media_rect,
            enhance: self.enhance,
        }
    }

    /// Copies these crop/enhance fields into an existing [`PhotoModifications`] without touching `paint`.
    pub fn apply_to(&self, mods: &mut PhotoModifications) {
        mods.angle = self.angle;
        mods.flipped = self.flipped;
        mods.crop = self.crop;
        mods.crop_type = self.crop_type;
        mods.crop_mode = self.crop_mode;
        mods.corners_level = self.corners_level;
        mods.canvas_media_rect = self.canvas_media_rect;
        mods.enhance = self.enhance;
    }
}

/// Multi-clip video timeline state persisted alongside a scene draft.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VideoTimelineDraft {
    /// Ordered sequence of video clips with per-clip trim, volume, and mute state.
    pub timeline: MultiClipTimeline,
    /// Per-overlay scene item visibility ranges on the global video timeline.
    #[serde(default)]
    pub overlay_visibility: OverlayVisibilityTrack,
    /// Selected output quality shorter side (`0` = Original).
    #[serde(default)]
    pub quality: i32,
    /// Send as silent looping GIF.
    #[serde(default)]
    pub gif: bool,
    /// Selected global cover-frame timestamp in milliseconds.
    #[serde(default)]
    pub cover_ms: i64,
    /// Optional overlay audio track attached to the timeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_track: Option<AudioTrackClip>,
}

impl VideoTimelineDraft {
    /// Validates clip invariants and overlay visibility ranges.
    pub fn validate(&self) -> Result<(), EditorError> {
        self.timeline.validate()?;
        self.overlay_visibility.validate()?;
        Ok(())
    }
}

/// Complete versioned editor draft containing scene items, crop/enhance parameters,
/// and an optional multi-clip video timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneDraft {
    /// Schema format version (must be `1..=SCENE_DRAFT_VERSION`).
    pub version: u32,
    /// Crop, rotation, flip, and auto-enhance settings.
    #[serde(default)]
    pub crop_enhance: CropEnhanceParams,
    /// Paint scene containing brush strokes, stickers, text, shapes, and story areas.
    #[serde(default)]
    pub scene: PaintScene,
    /// Optional multi-clip video timeline and overlay visibility schedule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_timeline: Option<VideoTimelineDraft>,
}

impl Default for SceneDraft {
    fn default() -> Self {
        Self {
            version: SCENE_DRAFT_VERSION,
            crop_enhance: CropEnhanceParams::default(),
            scene: PaintScene::default(),
            video_timeline: None,
        }
    }
}

impl SceneDraft {
    /// Creates a new versioned [`SceneDraft`] pinned to [`SCENE_DRAFT_VERSION`].
    #[must_use]
    pub fn new(
        crop_enhance: CropEnhanceParams,
        scene: PaintScene,
        video_timeline: Option<VideoTimelineDraft>,
    ) -> Self {
        Self {
            version: SCENE_DRAFT_VERSION,
            crop_enhance,
            scene,
            video_timeline,
        }
    }

    /// Validates that `version` is supported (`1..=SCENE_DRAFT_VERSION`).
    pub fn validate_version(version: u32) -> Result<(), EditorError> {
        if version == 0 || version > SCENE_DRAFT_VERSION {
            return Err(EditorError::UnsupportedDraftVersion {
                found: version,
                max_supported: SCENE_DRAFT_VERSION,
            });
        }
        Ok(())
    }

    /// Validates schema version and all nested timeline/visibility invariants.
    pub fn validate(&self) -> Result<(), EditorError> {
        Self::validate_version(self.version)?;
        if let Some(ref tl) = self.video_timeline {
            tl.validate()?;
        }
        Ok(())
    }

    /// Serializes this draft into a versioned UTF-8 JSON string.
    pub fn to_json(&self) -> Result<String, EditorError> {
        self.validate()?;
        serde_json::to_string(self).map_err(|e| EditorError::DraftSerialization(e.to_string()))
    }

    /// Deserializes a [`SceneDraft`] from a versioned JSON string.
    ///
    /// Checks the `"version"` field *before* parsing the full body so unsupported future
    /// versions always return [`EditorError::UnsupportedDraftVersion`].
    pub fn from_json(json: &str) -> Result<Self, EditorError> {
        let value: Value = serde_json::from_str(json)
            .map_err(|e| EditorError::DraftDeserialization(e.to_string()))?;
        let obj = value.as_object().ok_or_else(|| {
            EditorError::DraftDeserialization("draft root must be a JSON object".to_owned())
        })?;
        let raw_version = obj.get("version").and_then(Value::as_u64).ok_or_else(|| {
            EditorError::DraftDeserialization("missing or invalid integer 'version'".to_owned())
        })?;
        let version =
            u32::try_from(raw_version).map_err(|_| EditorError::UnsupportedDraftVersion {
                found: u32::MAX,
                max_supported: SCENE_DRAFT_VERSION,
            })?;
        Self::validate_version(version)?;

        let draft: Self = serde_json::from_str(json)
            .map_err(|e| EditorError::DraftDeserialization(e.to_string()))?;
        draft.validate()?;
        Ok(draft)
    }

    /// Serializes this draft into a versioned, checksum-protected binary envelope:
    ///
    /// `[magic: 4B "TGED"] [version: u32 LE] [payload_len: u32 LE] [checksum: u32 LE] [payload]`
    pub fn to_bytes(&self) -> Result<Vec<u8>, EditorError> {
        let json = self.to_json()?;
        let payload = json.as_bytes();
        if payload.len() > MAX_DRAFT_BINARY_BYTES {
            return Err(EditorError::DraftSerialization(format!(
                "draft payload size {} exceeds maximum {} bytes",
                payload.len(),
                MAX_DRAFT_BINARY_BYTES
            )));
        }
        let payload_len = u32::try_from(payload.len())
            .map_err(|_| EditorError::DraftSerialization("payload length overflow".to_owned()))?;
        let checksum = fnv1a_32(payload);

        let mut out = Vec::with_capacity(BINARY_HEADER_LEN + payload.len());
        out.extend_from_slice(&SCENE_DRAFT_BINARY_MAGIC);
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&payload_len.to_le_bytes());
        out.extend_from_slice(&checksum.to_le_bytes());
        out.extend_from_slice(payload);
        Ok(out)
    }

    /// Deserializes a [`SceneDraft`] from a binary envelope produced by [`Self::to_bytes`].
    ///
    /// Validates magic bytes, schema version, length cap, and payload checksum before decoding.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, EditorError> {
        if bytes.len() < BINARY_HEADER_LEN {
            return Err(EditorError::DraftDeserialization(format!(
                "binary draft too short: {} bytes (need at least {BINARY_HEADER_LEN})",
                bytes.len()
            )));
        }
        if bytes[0..4] != SCENE_DRAFT_BINARY_MAGIC {
            return Err(EditorError::DraftDeserialization(
                "invalid binary draft magic header".to_owned(),
            ));
        }
        let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        Self::validate_version(version)?;

        let payload_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
        if payload_len > MAX_DRAFT_BINARY_BYTES {
            return Err(EditorError::DraftDeserialization(format!(
                "binary draft payload length {payload_len} exceeds limit {MAX_DRAFT_BINARY_BYTES}"
            )));
        }
        let expected_checksum = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        let payload = &bytes[BINARY_HEADER_LEN..];
        if payload.len() != payload_len {
            return Err(EditorError::DraftDeserialization(format!(
                "binary draft payload length mismatch: header declares {payload_len}, found {}",
                payload.len()
            )));
        }
        let actual_checksum = fnv1a_32(payload);
        if actual_checksum != expected_checksum {
            return Err(EditorError::DraftDeserialization(format!(
                "binary draft checksum mismatch: expected 0x{expected_checksum:08x}, got 0x{actual_checksum:08x}"
            )));
        }
        let json = std::str::from_utf8(payload).map_err(|e| {
            EditorError::DraftDeserialization(format!("invalid UTF-8 payload: {e}"))
        })?;
        Self::from_json(json)
    }

    /// Computes a deterministic 64-bit FNV-1a fingerprint over scene items, crop/enhance params,
    /// multi-clip timeline, overlay visibility ranges, and audio track.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        fn feed(h: &mut u64, bytes: &[u8]) {
            for &b in bytes {
                *h ^= u64::from(b);
                *h = h.wrapping_mul(0x0100_0000_01b3);
            }
        }
        feed(&mut h, &self.version.to_le_bytes());
        feed(&mut h, &self.scene.fingerprint().to_le_bytes());
        feed(&mut h, &self.crop_enhance.angle.to_le_bytes());
        feed(&mut h, &[u8::from(self.crop_enhance.flipped)]);
        feed(&mut h, &self.crop_enhance.enhance.to_le_bytes());
        if let Some(crop) = self.crop_enhance.crop {
            feed(&mut h, &[1]);
            feed(&mut h, &crop.x.to_le_bytes());
            feed(&mut h, &crop.y.to_le_bytes());
            feed(&mut h, &crop.width.to_le_bytes());
            feed(&mut h, &crop.height.to_le_bytes());
        } else {
            feed(&mut h, &[0]);
        }
        if let Some(ref vt) = self.video_timeline {
            feed(&mut h, &[1]);
            feed(&mut h, &vt.quality.to_le_bytes());
            feed(&mut h, &[u8::from(vt.gif)]);
            feed(&mut h, &vt.cover_ms.to_le_bytes());
            for clip in vt.timeline.clips() {
                feed(&mut h, &clip.id.0.to_le_bytes());
                feed(&mut h, &clip.duration_ms().to_le_bytes());
                feed(&mut h, &clip.trim.from_ms.to_le_bytes());
                feed(&mut h, &clip.trim.till_ms.to_le_bytes());
                feed(&mut h, &clip.volume.to_bits().to_le_bytes());
                feed(&mut h, &[u8::from(clip.muted), u8::from(clip.has_audio())]);
            }
            for (item_id, range) in vt.overlay_visibility.entries() {
                feed(&mut h, &item_id.0.to_le_bytes());
                feed(&mut h, &range.start_ms.to_le_bytes());
                feed(&mut h, &range.end_ms.to_le_bytes());
            }
            if let Some(ref audio) = vt.audio_track {
                feed(&mut h, &[1]);
                feed(&mut h, audio.title.as_bytes());
                feed(&mut h, audio.performer.as_bytes());
                feed(&mut h, &audio.total_duration_ms.to_le_bytes());
                feed(&mut h, &audio.trim_from_ms.to_le_bytes());
                feed(&mut h, &audio.trim_till_ms.to_le_bytes());
                feed(&mut h, &audio.volume.to_bits().to_le_bytes());
                feed(&mut h, &[u8::from(audio.muted)]);
            } else {
                feed(&mut h, &[0]);
            }
        } else {
            feed(&mut h, &[0]);
        }
        h
    }
}

fn fnv1a_32(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

// ---------------------------------------------------------------------------
// tdesktop-compatible `SceneItems` flat draft schema (`scene_draft.{h,cpp}`)
// ---------------------------------------------------------------------------

/// Flat sticker draft entry (`Editor::SceneItemSticker`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftSticker {
    /// Sticker document reference.
    pub sticker: StickerRef,
    /// Canvas placement.
    pub placement: ItemPlacement,
}

/// Flat text draft entry (`Editor::SceneItemText`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftText {
    /// Text string.
    pub text: String,
    /// Primary color.
    pub color: Color,
    /// Font size.
    pub font_size: f64,
    /// Visual style.
    pub style: TextStyle,
    /// Typeface family.
    pub typeface: TextTypeface,
    /// Paragraph alignment.
    pub alignment: TextAlignment,
    /// Canvas placement.
    pub placement: ItemPlacement,
}

/// Flat vector shape draft entry (`Editor::SceneItemShape`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftShape {
    /// Shape kind.
    pub shape: ShapeType,
    /// Stroke/fill color.
    pub color: Color,
    /// Stroke width.
    pub stroke_width: f64,
    /// Whether shape interior is filled.
    pub fill: bool,
    /// Arrow bend factor.
    pub bend: f64,
    /// Canvas placement.
    pub placement: ItemPlacement,
}

/// Flat video overlay draft entry (`Editor::SceneItemVideo`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftVideo {
    /// External file index/path string.
    pub file: String,
    /// Source duration in milliseconds.
    pub duration_ms: i64,
    /// Round video flag.
    pub round: bool,
    /// Whether clip acts as canvas background.
    pub background: bool,
    /// Whether background is locked.
    pub background_locked: bool,
    /// Whether source has audio.
    pub has_audio: bool,
    /// Trim start in milliseconds.
    pub from_ms: i64,
    /// Trim end in milliseconds.
    pub till_ms: i64,
    /// Audio volume in `0.0..=1.0`.
    pub volume: f64,
    /// Canvas placement.
    pub placement: ItemPlacement,
}

/// Flat story link draft entry (`Editor::SceneItemLink`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftLink {
    /// Target URL.
    pub url: String,
    /// Display title.
    pub title: String,
    /// Pill style.
    pub style: LinkPillStyle,
    /// Canvas placement.
    pub placement: ItemPlacement,
}

/// Flat reaction draft entry (`Editor::SceneItemReaction`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftReaction {
    /// Emoji glyph or custom emoji ID.
    pub reaction: String,
    /// Dark bubble flag.
    pub dark: bool,
    /// Canvas placement.
    pub placement: ItemPlacement,
}

/// Flat location draft entry (`Editor::SceneItemLocation`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftLocation {
    /// Geo/venue data.
    pub location: LocationData,
    /// Pill style.
    pub style: LinkPillStyle,
    /// Canvas placement.
    pub placement: ItemPlacement,
}

/// Flat weather draft entry (`Editor::SceneItemWeather`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftWeather {
    /// Weather metadata.
    pub weather: WeatherData,
    /// Canvas placement.
    pub placement: ItemPlacement,
}

/// Flat reposted message card draft entry (`Editor::SceneItemMessage`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftMessage {
    /// Channel peer ID.
    pub channel_id: i64,
    /// Message ID.
    pub msg_id: i32,
    /// Optional dark theme override.
    pub dark: Option<bool>,
    /// Canvas placement.
    pub placement: ItemPlacement,
}

/// Flat structured collection of scene items matching tdesktop's `Editor::SceneItems`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SceneItemsDraft {
    /// Sticker items.
    pub stickers: Vec<DraftSticker>,
    /// Text items.
    pub texts: Vec<DraftText>,
    /// Vector shape items.
    pub shapes: Vec<DraftShape>,
    /// Embedded video items.
    pub videos: Vec<DraftVideo>,
    /// Story URL link items.
    pub links: Vec<DraftLink>,
    /// Story reaction items.
    pub reactions: Vec<DraftReaction>,
    /// Location pill items.
    pub locations: Vec<DraftLocation>,
    /// Weather pill items.
    pub weather: Vec<DraftWeather>,
    /// Reposted message items.
    pub messages: Vec<DraftMessage>,
}

impl SceneItemsDraft {
    /// Returns `true` when all item lists are empty (`SceneItems::empty`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stickers.is_empty()
            && self.texts.is_empty()
            && self.shapes.is_empty()
            && self.videos.is_empty()
            && self.links.is_empty()
            && self.reactions.is_empty()
            && self.locations.is_empty()
            && self.weather.is_empty()
            && self.messages.is_empty()
    }

    /// Returns the maximum z-value across all items (`SceneItems::maxZ`).
    #[must_use]
    pub fn max_z(&self) -> f64 {
        let mut max_z = 0.0f64;
        for s in &self.stickers {
            max_z = max_z.max(s.placement.z_value);
        }
        for t in &self.texts {
            max_z = max_z.max(t.placement.z_value);
        }
        for s in &self.shapes {
            max_z = max_z.max(s.placement.z_value);
        }
        for v in &self.videos {
            max_z = max_z.max(v.placement.z_value);
        }
        for l in &self.links {
            max_z = max_z.max(l.placement.z_value);
        }
        for r in &self.reactions {
            max_z = max_z.max(r.placement.z_value);
        }
        for loc in &self.locations {
            max_z = max_z.max(loc.placement.z_value);
        }
        for w in &self.weather {
            max_z = max_z.max(w.placement.z_value);
        }
        for m in &self.messages {
            max_z = max_z.max(m.placement.z_value);
        }
        max_z
    }
}

/// Extracts active scene items from `scene` into a flat [`SceneItemsDraft`]
/// (`Editor::CollectSceneItems`).
#[must_use]
pub fn collect_scene_items(scene: &PaintScene) -> SceneItemsDraft {
    let mut result = SceneItemsDraft::default();
    for entry in scene.active_items_by_z() {
        if entry.status != ItemStatus::Normal {
            continue;
        }
        match &entry.item {
            SceneItemKind::Sticker(s) => result.stickers.push(DraftSticker {
                sticker: s.sticker.clone(),
                placement: s.placement,
            }),
            SceneItemKind::Text(t) => result.texts.push(DraftText {
                text: t.text.clone(),
                color: t.color,
                font_size: t.font_size,
                style: t.style,
                typeface: t.typeface,
                alignment: t.alignment,
                placement: t.placement,
            }),
            SceneItemKind::Shape(s) => result.shapes.push(DraftShape {
                shape: s.shape,
                color: s.color,
                stroke_width: s.stroke_width,
                fill: s.fill,
                bend: s.bend,
                placement: s.placement,
            }),
            SceneItemKind::Location(l) => result.locations.push(DraftLocation {
                location: l.location.clone(),
                style: l.style,
                placement: l.placement,
            }),
            SceneItemKind::Weather(w) => result.weather.push(DraftWeather {
                weather: w.weather.clone(),
                placement: w.placement,
            }),
            SceneItemKind::StoryLink(l) => result.links.push(DraftLink {
                url: l.url.clone(),
                title: l.title.clone(),
                style: l.style,
                placement: l.placement,
            }),
            SceneItemKind::StoryReaction(r) => result.reactions.push(DraftReaction {
                reaction: r.reaction.clone(),
                dark: r.dark,
                placement: r.placement,
            }),
            SceneItemKind::StoryChannelPost(p) => result.messages.push(DraftMessage {
                channel_id: p.channel_id,
                msg_id: p.msg_id,
                dark: None,
                placement: p.placement,
            }),
            SceneItemKind::Line(_)
            | SceneItemKind::Eraser(_)
            | SceneItemKind::Placement(_)
            | SceneItemKind::ZOrder(_)
            | SceneItemKind::Remove(_) => {}
        }
    }
    result
}

fn serialize_placement(p: &ItemPlacement) -> Value {
    json!([
        p.position.x,
        p.position.y,
        p.rotation,
        p.scale,
        p.z_value,
        p.size,
        p.aspect_ratio,
        if p.flipped { 1 } else { 0 }
    ])
}

fn deserialize_placement(arr: Option<&Vec<Value>>) -> ItemPlacement {
    let Some(arr) = arr else {
        return ItemPlacement::default();
    };
    if arr.len() < 7 {
        return ItemPlacement::default();
    }
    ItemPlacement {
        position: PointF::new(
            arr[0].as_f64().unwrap_or(0.0),
            arr[1].as_f64().unwrap_or(0.0),
        ),
        rotation: arr[2].as_f64().unwrap_or(0.0),
        scale: arr[3].as_f64().unwrap_or(1.0),
        z_value: arr[4].as_f64().unwrap_or(0.0),
        size: arr[5].as_f64().unwrap_or(0.0),
        aspect_ratio: arr[6].as_f64().unwrap_or(1.0),
        flipped: arr.get(7).and_then(Value::as_i64).is_some_and(|v| v == 1),
    }
}

fn link_style_to_int(style: LinkPillStyle) -> i32 {
    match style {
        LinkPillStyle::Light => 0,
        LinkPillStyle::Dark => 1,
        LinkPillStyle::Transparent => 2,
        LinkPillStyle::Color => 3,
    }
}

fn link_style_from_int(v: i64) -> LinkPillStyle {
    match v {
        1 => LinkPillStyle::Dark,
        2 => LinkPillStyle::Transparent,
        3 => LinkPillStyle::Color,
        _ => LinkPillStyle::Light,
    }
}

fn shape_type_from_int(v: i64) -> ShapeType {
    match v {
        1 => ShapeType::Rectangle,
        2 => ShapeType::Star,
        3 => ShapeType::Bubble,
        4 => ShapeType::Arrow,
        _ => ShapeType::Circle,
    }
}

/// Serializes [`SceneItemsDraft`] into a tdesktop-compatible JSON array
/// (`Editor::SerializeSceneItems`).
#[must_use]
pub fn serialize_scene_items_json(items: &SceneItemsDraft) -> Value {
    let mut array = Vec::new();
    for s in &items.stickers {
        array.push(json!({
            "kind": "sticker",
            "doc": s.sticker.document_id,
            "set": s.sticker.set_short_name,
            "setId": s.sticker.set_id.to_string(),
            "setHash": s.sticker.set_access_hash.to_string(),
            "p": serialize_placement(&s.placement),
        }));
    }
    for t in &items.texts {
        array.push(json!({
            "kind": "text",
            "text": t.text,
            "color": t.color.to_hex_argb(),
            "size": t.font_size,
            "style": t.style as u8,
            "typeface": t.typeface as u8,
            "align": t.alignment as u8,
            "p": serialize_placement(&t.placement),
        }));
    }
    for s in &items.shapes {
        array.push(json!({
            "kind": "shape",
            "shape": s.shape as u8,
            "color": s.color.to_hex_argb(),
            "stroke": s.stroke_width,
            "fill": s.fill,
            "bend": s.bend,
            "p": serialize_placement(&s.placement),
        }));
    }
    for v in &items.videos {
        array.push(json!({
            "kind": "video",
            "file": v.file,
            "duration": v.duration_ms,
            "round": v.round,
            "background": v.background,
            "locked": v.background_locked,
            "audio": v.has_audio,
            "from": v.from_ms,
            "till": v.till_ms,
            "vol": v.volume,
            "p": serialize_placement(&v.placement),
        }));
    }
    for l in &items.links {
        array.push(json!({
            "kind": "link",
            "preview": {
                "url": l.url,
                "name": l.title,
            },
            "style": link_style_to_int(l.style),
            "p": serialize_placement(&l.placement),
        }));
    }
    for r in &items.reactions {
        array.push(json!({
            "kind": "reaction",
            "reaction": { "emoji": r.reaction },
            "dark": r.dark,
            "p": serialize_placement(&r.placement),
        }));
    }
    for loc in &items.locations {
        array.push(json!({
            "kind": "location",
            "loc": {
                "lat": loc.location.lat,
                "lon": loc.location.lon,
                "title": loc.location.title,
                "address": loc.location.address,
                "country": loc.location.country_iso2,
                "city": loc.location.city,
                "provider": loc.location.provider,
                "venueId": loc.location.venue_id,
                "venueType": loc.location.venue_type,
            },
            "style": link_style_to_int(loc.style),
            "p": serialize_placement(&loc.placement),
        }));
    }
    for w in &items.weather {
        array.push(json!({
            "kind": "weather",
            "weather": {
                "emoji": w.weather.emoji,
                "celsius": w.weather.celsius,
                "color": w.weather.color.to_hex_argb(),
            },
            "p": serialize_placement(&w.placement),
        }));
    }
    for m in &items.messages {
        let dark_int = match m.dark {
            Some(true) => 1,
            Some(false) => 0,
            None => -1,
        };
        array.push(json!({
            "kind": "message",
            "peer": m.channel_id.to_string(),
            "msgid": m.msg_id,
            "dark": dark_int,
            "p": serialize_placement(&m.placement),
        }));
    }
    Value::Array(array)
}

fn get_str(obj: &Map<String, Value>, key: &str) -> String {
    obj.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Parses a tdesktop-compatible JSON array produced by [`serialize_scene_items_json`]
/// (`Editor::ParseSceneItems`).
pub fn parse_scene_items_json(value: &Value) -> Result<SceneItemsDraft, EditorError> {
    let entries = value.as_array().ok_or_else(|| {
        EditorError::DraftDeserialization("scene items payload must be a JSON array".to_owned())
    })?;
    let mut result = SceneItemsDraft::default();
    for entry in entries {
        let Some(obj) = entry.as_object() else {
            continue;
        };
        let kind = get_str(obj, "kind");
        let placement = deserialize_placement(obj.get("p").and_then(Value::as_array));
        match kind.as_str() {
            "sticker" => {
                result.stickers.push(DraftSticker {
                    sticker: StickerRef {
                        document_id: obj.get("doc").and_then(Value::as_u64).unwrap_or(0),
                        set_short_name: get_str(obj, "set"),
                        set_id: get_str(obj, "setId").parse().unwrap_or(0),
                        set_access_hash: get_str(obj, "setHash").parse().unwrap_or(0),
                    },
                    placement,
                });
            }
            "text" => {
                result.texts.push(DraftText {
                    text: get_str(obj, "text"),
                    color: Color::from_hex_argb(&get_str(obj, "color")).unwrap_or(Color::WHITE),
                    font_size: obj.get("size").and_then(Value::as_f64).unwrap_or(24.0),
                    style: TextStyle::from_serialized(
                        obj.get("style").and_then(Value::as_i64).unwrap_or(2) as i32,
                    ),
                    typeface: TextTypeface::from_serialized(
                        obj.get("typeface").and_then(Value::as_i64).unwrap_or(0) as i32,
                    ),
                    alignment: TextAlignment::from_serialized(
                        obj.get("align").and_then(Value::as_i64).unwrap_or(0) as i32,
                    ),
                    placement,
                });
            }
            "shape" => {
                result.shapes.push(DraftShape {
                    shape: shape_type_from_int(
                        obj.get("shape").and_then(Value::as_i64).unwrap_or(0),
                    ),
                    color: Color::from_hex_argb(&get_str(obj, "color")).unwrap_or(Color::WHITE),
                    stroke_width: obj.get("stroke").and_then(Value::as_f64).unwrap_or(1.0),
                    fill: obj.get("fill").and_then(Value::as_bool).unwrap_or(false),
                    bend: obj.get("bend").and_then(Value::as_f64).unwrap_or(0.0),
                    placement,
                });
            }
            "video" => {
                result.videos.push(DraftVideo {
                    file: get_str(obj, "file"),
                    duration_ms: obj.get("duration").and_then(Value::as_i64).unwrap_or(0),
                    round: obj.get("round").and_then(Value::as_bool).unwrap_or(false),
                    background: obj
                        .get("background")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    background_locked: obj.get("locked").and_then(Value::as_bool).unwrap_or(false),
                    has_audio: obj.get("audio").and_then(Value::as_bool).unwrap_or(false),
                    from_ms: obj.get("from").and_then(Value::as_i64).unwrap_or(0),
                    till_ms: obj.get("till").and_then(Value::as_i64).unwrap_or(0),
                    volume: obj.get("vol").and_then(Value::as_f64).unwrap_or(1.0),
                    placement,
                });
            }
            "link" => {
                let preview = obj.get("preview").and_then(Value::as_object);
                result.links.push(DraftLink {
                    url: preview.map(|p| get_str(p, "url")).unwrap_or_default(),
                    title: preview.map(|p| get_str(p, "name")).unwrap_or_default(),
                    style: link_style_from_int(
                        obj.get("style").and_then(Value::as_i64).unwrap_or(0),
                    ),
                    placement,
                });
            }
            "reaction" => {
                let reaction_obj = obj.get("reaction").and_then(Value::as_object);
                let reaction = reaction_obj
                    .and_then(|r| {
                        r.get("custom")
                            .and_then(Value::as_i64)
                            .map(|id| id.to_string())
                            .or_else(|| r.get("emoji").and_then(Value::as_str).map(str::to_owned))
                    })
                    .unwrap_or_default();
                result.reactions.push(DraftReaction {
                    reaction,
                    dark: obj.get("dark").and_then(Value::as_bool).unwrap_or(false),
                    placement,
                });
            }
            "location" => {
                let loc = obj.get("loc").and_then(Value::as_object);
                let location = if let Some(loc) = loc {
                    LocationData {
                        lat: loc.get("lat").and_then(Value::as_f64).unwrap_or(0.0),
                        lon: loc.get("lon").and_then(Value::as_f64).unwrap_or(0.0),
                        title: get_str(loc, "title"),
                        address: get_str(loc, "address"),
                        country_iso2: get_str(loc, "country"),
                        city: get_str(loc, "city"),
                        provider: get_str(loc, "provider"),
                        venue_id: get_str(loc, "venueId"),
                        venue_type: get_str(loc, "venueType"),
                    }
                } else {
                    LocationData::default()
                };
                result.locations.push(DraftLocation {
                    location,
                    style: link_style_from_int(
                        obj.get("style").and_then(Value::as_i64).unwrap_or(0),
                    ),
                    placement,
                });
            }
            "weather" => {
                let w = obj.get("weather").and_then(Value::as_object);
                let weather = if let Some(w) = w {
                    WeatherData {
                        emoji: get_str(w, "emoji"),
                        celsius: w.get("celsius").and_then(Value::as_f64).unwrap_or(0.0),
                        color: Color::from_hex_argb(&get_str(w, "color"))
                            .unwrap_or(Color::rgb(64, 167, 227)),
                    }
                } else {
                    WeatherData::default()
                };
                result.weather.push(DraftWeather { weather, placement });
            }
            "message" => {
                let dark = match obj.get("dark").and_then(Value::as_i64).unwrap_or(-1) {
                    1 => Some(true),
                    0 => Some(false),
                    _ => None,
                };
                result.messages.push(DraftMessage {
                    channel_id: get_str(obj, "peer").parse().unwrap_or(0),
                    msg_id: obj.get("msgid").and_then(Value::as_i64).unwrap_or(0) as i32,
                    dark,
                    placement,
                });
            }
            _ => {}
        }
    }
    Ok(result)
}
