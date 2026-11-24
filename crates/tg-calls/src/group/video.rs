//! Group call video endpoints, SSRC groups, and quality budget downgrade algorithm.
//!
//! Port of `calls/group/calls_group_call.h` (`VideoEndpointType`, `VideoEndpoint`,
//! `VideoStateToggle`, `VideoQualityRequest`) and `calls/group/calls_group_call.cpp`
//! (`VideoParams`, `ParticipantVideoParams`, `ParseVideoParams`,
//! `updateRequestedVideoChannels`).

use std::cmp::Ordering;

use crate::group::{FULL_AS_MEDIUMS_COUNT, MAX_MEDIUM_QUALITIES, RTMP_ENDPOINT_ID};
use crate::types::{GroupCallParticipantVideo, PeerId};

/// Type of video stream endpoint (`Calls::VideoEndpointType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VideoEndpointType {
    /// Participant webcam feed.
    Camera,
    /// Participant screen share (or RTMP broadcast unified stream).
    Screen,
}

/// Requested video quality tier (`Calls::Group::VideoQuality`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VideoQuality {
    /// Low-resolution grid thumbnail.
    #[default]
    Thumbnail,
    /// Medium resolution for multi-tile layout.
    Medium,
    /// Full resolution for large/pinned view or screen shares.
    Full,
}

/// Identifies a participant's camera or screen video stream (`Calls::VideoEndpoint`).
///
/// Mirrors `calls_group_call.h:101-162`: equality is determined by `id`, while ordering
/// compares `(peer, id)` so tracks sort stably by peer.
#[derive(Debug, Clone)]
pub struct VideoEndpoint {
    /// Camera or screen share.
    pub kind: VideoEndpointType,
    /// Owning peer ID (present whenever `id` is non-empty).
    pub peer: Option<PeerId>,
    /// Endpoint identifier string (empty means null/unset endpoint).
    pub id: String,
}

impl Default for VideoEndpoint {
    fn default() -> Self {
        Self {
            kind: VideoEndpointType::Camera,
            peer: None,
            id: String::new(),
        }
    }
}

impl VideoEndpoint {
    /// Construct a non-empty video endpoint.
    pub fn new(kind: VideoEndpointType, peer: PeerId, id: impl Into<String>) -> Self {
        Self {
            kind,
            peer: Some(peer),
            id: id.into(),
        }
    }

    /// Returns `true` if this is the synthetic RTMP `"unified"` endpoint.
    pub fn is_rtmp(&self) -> bool {
        self.id == RTMP_ENDPOINT_ID
    }

    /// Returns `true` if `id` is empty.
    pub fn is_empty(&self) -> bool {
        self.id.is_empty()
    }
}

impl PartialEq for VideoEndpoint {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for VideoEndpoint {}

impl PartialOrd for VideoEndpoint {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for VideoEndpoint {
    fn cmp(&self, other: &Self) -> Ordering {
        self.peer
            .cmp(&other.peer)
            .then_with(|| self.id.cmp(&other.id))
    }
}

/// Media SSRC group (`tgcalls::MediaSsrcGroup`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSsrcGroup {
    /// Semantics (e.g. `"SIM"`, `"FID"`).
    pub semantics: String,
    /// Non-empty list of SSRCs.
    pub ssrcs: Vec<u32>,
}

/// Parsed single video stream parameters (`VideoParams` in `calls_group_call.cpp:281-293`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VideoParams {
    /// Video endpoint ID.
    pub endpoint_id: String,
    /// SSRC groups (empty source groups are filtered out during parsing).
    pub ssrc_groups: Vec<MediaSsrcGroup>,
    /// Additional audio SSRC (used for screen-share audio).
    pub additional_ssrc: u32,
    /// Whether the video stream is paused.
    pub paused: bool,
}

impl VideoParams {
    /// Matches `VideoParams::empty()` (`calls_group_call.cpp:287-289`).
    pub fn is_empty(&self) -> bool {
        self.additional_ssrc == 0 && (self.endpoint_id.is_empty() || self.ssrc_groups.is_empty())
    }
}

/// Combined camera and screen-share video parameters for a participant
/// (`Calls::ParticipantVideoParams`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParticipantVideoParams {
    /// Camera video parameters.
    pub camera: VideoParams,
    /// Screen-share presentation video parameters.
    pub screen: VideoParams,
}

impl ParticipantVideoParams {
    /// Parse a single TL `GroupCallParticipantVideo` into [`VideoParams`],
    /// dropping any source group with an empty `sources` list (matching `calls_group_call.cpp:367-369`).
    pub fn parse_single(params: Option<&GroupCallParticipantVideo>) -> VideoParams {
        let Some(data) = params else {
            return VideoParams::default();
        };
        let ssrc_groups = data
            .source_groups
            .iter()
            .filter(|g| !g.sources.is_empty())
            .map(|g| MediaSsrcGroup {
                semantics: g.semantics.clone(),
                ssrcs: g.sources.clone(),
            })
            .collect();
        VideoParams {
            endpoint_id: data.endpoint.clone(),
            ssrc_groups,
            additional_ssrc: data.audio_source.unwrap_or(0),
            paused: data.paused,
        }
    }

    /// Parse camera and screen TL video parameters, returning `None` when both are absent.
    ///
    /// Port of `ParseVideoParams` (`calls_group_call.cpp:408-430`).
    pub fn parse(
        camera: Option<&GroupCallParticipantVideo>,
        screen: Option<&GroupCallParticipantVideo>,
    ) -> Option<Self> {
        if camera.is_none() && screen.is_none() {
            return None;
        }
        Some(Self {
            camera: Self::parse_single(camera),
            screen: Self::parse_single(screen),
        })
    }

    /// Camera endpoint ID (empty string if absent).
    pub fn camera_endpoint(&self) -> &str {
        &self.camera.endpoint_id
    }

    /// Screen-share endpoint ID (empty string if absent).
    pub fn screen_endpoint(&self) -> &str {
        &self.screen.endpoint_id
    }

    /// Additional audio SSRC from screen-sharing (`GetAdditionalAudioSsrc`).
    pub fn additional_audio_ssrc(&self) -> u32 {
        self.screen.additional_ssrc
    }
}

/// Requested video channel passed to the media engine (`tgcalls::VideoChannelDescription`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoChannelDescription {
    /// Participant audio SSRC (0 for RTMP unified stream).
    pub audio_ssrc: u32,
    /// Participant user ID (0 for non-user or RTMP).
    pub user_id: i64,
    /// Video endpoint ID.
    pub endpoint_id: String,
    /// SSRC groups for this video stream.
    pub ssrc_groups: Vec<MediaSsrcGroup>,
    /// Minimum acceptable quality tier.
    pub min_quality: VideoQuality,
    /// Maximum requested quality tier.
    pub max_quality: VideoQuality,
}

/// Input track entry for [`compute_requested_video_channels`].
#[derive(Debug, Clone)]
pub struct RequestedVideoTrackInput<'a> {
    /// Video endpoint.
    pub endpoint: &'a VideoEndpoint,
    /// Desired UI quality tier.
    pub desired_quality: VideoQuality,
    /// Participant audio SSRC (ignored for RTMP).
    pub audio_ssrc: u32,
    /// Participant user ID (0 if non-user).
    pub user_id: i64,
    /// Participant SSRC groups for this endpoint.
    pub ssrc_groups: &'a [MediaSsrcGroup],
}

/// Build the list of [`VideoChannelDescription`]s and enforce tdesktop's bandwidth/quality budget.
///
/// Exact port of `GroupCall::updateRequestedVideoChannels` (`calls_group_call.cpp:3379-3484`):
/// - Skips the local user's own `camera_sharing_endpoint` and `screen_sharing_endpoint`.
/// - Limits `count(Full) * 4 + count(Medium) <= 16`:
///   1. If over budget, downgrades all `Full` cameras (`min_quality != Full`) to `Medium`.
///   2. If `full_screencasts * 4 > 16`, also downgrades all `Full` screencasts to `Thumbnail`.
///   3. If resulting `mediums > 16`, downgrades all `Medium` channels to `Thumbnail`.
pub fn compute_requested_video_channels(
    tracks: &[RequestedVideoTrackInput<'_>],
    self_camera_endpoint: &str,
    self_screen_endpoint: &str,
) -> Vec<VideoChannelDescription> {
    let mut channels = Vec::with_capacity(tracks.len());
    let mut mediums: usize = 0;
    let mut full_cameras: usize = 0;
    let mut full_screencasts: usize = 0;

    for track in tracks {
        let endpoint_id = track.endpoint.id.as_str();
        if endpoint_id.is_empty()
            || endpoint_id == self_camera_endpoint
            || endpoint_id == self_screen_endpoint
        {
            continue;
        }
        if endpoint_id == RTMP_ENDPOINT_ID {
            channels.push(VideoChannelDescription {
                audio_ssrc: 0,
                user_id: 0,
                endpoint_id: endpoint_id.to_string(),
                ssrc_groups: Vec::new(),
                min_quality: if track.desired_quality == VideoQuality::Full {
                    VideoQuality::Full
                } else {
                    VideoQuality::Thumbnail
                },
                max_quality: VideoQuality::Full,
            });
            continue;
        }

        if track.audio_ssrc == 0 {
            continue;
        }

        let is_screen = track.endpoint.kind == VideoEndpointType::Screen;
        let min_quality = if track.desired_quality == VideoQuality::Full && is_screen {
            VideoQuality::Full
        } else {
            VideoQuality::Thumbnail
        };
        let max_quality = if track.desired_quality == VideoQuality::Full {
            VideoQuality::Full
        } else if track.desired_quality == VideoQuality::Medium && !is_screen {
            VideoQuality::Medium
        } else {
            VideoQuality::Thumbnail
        };

        match max_quality {
            VideoQuality::Full => {
                if is_screen {
                    full_screencasts += 1;
                } else {
                    full_cameras += 1;
                }
            }
            VideoQuality::Medium => {
                mediums += 1;
            }
            VideoQuality::Thumbnail => {}
        }

        channels.push(VideoChannelDescription {
            audio_ssrc: track.audio_ssrc,
            user_id: track.user_id,
            endpoint_id: endpoint_id.to_string(),
            ssrc_groups: track.ssrc_groups.to_vec(),
            min_quality,
            max_quality,
        });
    }

    let mediums_count = mediums + (full_cameras + full_screencasts) * FULL_AS_MEDIUMS_COUNT;
    let downgrade_some = mediums_count > MAX_MEDIUM_QUALITIES;
    let downgrade_all = (full_screencasts * FULL_AS_MEDIUMS_COUNT) > MAX_MEDIUM_QUALITIES;

    if downgrade_some {
        for channel in &mut channels {
            if channel.max_quality == VideoQuality::Full && channel.endpoint_id != RTMP_ENDPOINT_ID
            {
                let is_camera = channel.min_quality != VideoQuality::Full;
                if is_camera {
                    channel.max_quality = VideoQuality::Medium;
                } else if downgrade_all {
                    channel.max_quality = VideoQuality::Thumbnail;
                    channel.min_quality = VideoQuality::Thumbnail;
                }
            }
        }
        mediums += full_cameras;
    }

    if mediums > MAX_MEDIUM_QUALITIES {
        for channel in &mut channels {
            if channel.max_quality == VideoQuality::Medium {
                channel.max_quality = VideoQuality::Thumbnail;
            }
        }
    }

    channels
}
