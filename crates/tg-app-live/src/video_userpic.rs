//! Live loader for animated video avatars in the dialogs list (WP-40).
//!
//! Port origin:
//! - `Telegram/SourceFiles/dialogs/dialogs_inner_widget.cpp` (`validateVideoUserpic`)
//! - `Telegram/SourceFiles/dialogs/ui/dialogs_video_userpic.cpp`
//! - `Telegram/SourceFiles/data/data_photo.cpp` (`videoContent`, `VideoSize` types `u` / `v`)
//!
//! Rows ask for a peer's video avatar every frame. The first request starts a background thread
//! that fetches the full user (`users.getFullUser`), downloads the video size of the matching
//! profile photo (`upload.getFile` on `inputPhotoFileLocation`), and decodes it into frames.
//! Later requests return `Pending` until the frames are ready.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tg_api::RpcClient;
use tg_api::convert::input_user_for;
use tg_tl::{enums as tl_enums, functions as tl_fns, types as tl_types};
use tokio::runtime::Handle;

use crate::bridge::LiveBridgeState;
use crate::convert::to_data_peer;
use crate::media::{block_on_runtime, download_file_bytes};
use crate::session::LiveSession;
use tg_app_core::core_types::video_userpic::{
    VIDEO_USERPIC_DECODE_SIZE, VideoUserpicFetch, VideoUserpicFrame, decode_video_userpic_bytes,
};
use tg_app_core::stand_in::types::PeerId;

/// Decodes downloaded video bytes into frames (replaceable in tests).
pub type VideoUserpicDecoder = fn(&[u8]) -> Result<Vec<VideoUserpicFrame>, String>;

fn default_decoder(bytes: &[u8]) -> Result<Vec<VideoUserpicFrame>, String> {
    decode_video_userpic_bytes(bytes, VIDEO_USERPIC_DECODE_SIZE)
}

enum LoadState {
    Pending,
    Ready(Vec<VideoUserpicFrame>),
    Failed,
}

struct Entry {
    owner: Weak<LiveBridgeState>,
    decoder: VideoUserpicDecoder,
    loads: HashMap<i64, LoadState>,
}

fn registry() -> &'static Mutex<HashMap<usize, Entry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<usize, Entry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn registry_key(live: &LiveSession) -> usize {
    Arc::as_ptr(live.bridge_state()) as usize
}

fn with_entry<R>(live: &LiveSession, f: impl FnOnce(&mut Entry) -> R) -> R {
    let key = registry_key(live);
    let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, entry| entry.owner.strong_count() > 0);
    let entry = map.entry(key).or_insert_with(|| Entry {
        owner: Arc::downgrade(live.bridge_state()),
        decoder: default_decoder,
        loads: HashMap::new(),
    });
    f(entry)
}

/// Replaces the video decoder for `live` (tests use a fake that needs no FFmpeg).
#[doc(hidden)]
pub fn set_video_userpic_decoder_live(live: &LiveSession, decoder: VideoUserpicDecoder) {
    with_entry(live, |e| e.decoder = decoder);
}

/// Photo ID of `peer_id`'s video avatar when its dialogs row should animate it
/// (`InnerWidget::validateVideoUserpic`: premium user, known photo with video, not self).
pub(crate) fn dialog_video_userpic_live(live: &LiveSession, peer_id: PeerId) -> Option<i64> {
    let data_peer = to_data_peer(peer_id);
    live.read_session(move |s| {
        let peer = s.peer(data_peer)?;
        let user = peer.as_user()?;
        let common = peer.common();
        let photo = common.userpic_photo_id;
        let known = photo != tg_data::UNKNOWN_PHOTO_ID && photo.0 != 0;
        (user.is_premium() && !user.is_self() && common.userpic_has_video && known)
            .then_some(photo.0 as i64)
    })
}

/// Frames of `peer_id`'s video avatar `photo_id`; starts the download on the first request.
pub(crate) fn video_userpic_frames_live(
    live: &LiveSession,
    peer_id: PeerId,
    photo_id: i64,
) -> VideoUserpicFetch {
    enum Next {
        Return(VideoUserpicFetch),
        Start(VideoUserpicDecoder),
    }
    let next = with_entry(live, |e| match e.loads.remove(&photo_id) {
        None => {
            e.loads.insert(photo_id, LoadState::Pending);
            Next::Start(e.decoder)
        }
        Some(LoadState::Pending) => {
            e.loads.insert(photo_id, LoadState::Pending);
            Next::Return(VideoUserpicFetch::Pending)
        }
        Some(LoadState::Ready(frames)) => Next::Return(VideoUserpicFetch::Ready(frames)),
        Some(LoadState::Failed) => {
            e.loads.insert(photo_id, LoadState::Failed);
            Next::Return(VideoUserpicFetch::Failed)
        }
    });
    let decoder = match next {
        Next::Return(fetch) => return fetch,
        Next::Start(decoder) => decoder,
    };

    let data_peer = to_data_peer(peer_id);
    let input_user = live.read_session(move |s| {
        data_peer
            .to_user()
            .map(|uid| input_user_for(s, uid))
            .unwrap_or(tl_enums::InputUser::Empty)
    });
    let key = registry_key(live);
    let bridge = Arc::downgrade(live.bridge_state());
    let rpc = live.runtime().rpc().clone();
    let handle = live.runtime().tokio_handle().clone();
    let spawned = std::thread::Builder::new()
        .name("rustgram-video-userpic".to_owned())
        .spawn(move || {
            let state = match load_video_userpic(&rpc, &handle, input_user, photo_id)
                .and_then(|bytes| decoder(&bytes))
            {
                Ok(frames) if !frames.is_empty() => LoadState::Ready(frames),
                _ => LoadState::Failed,
            };
            if let Ok(mut map) = registry().lock()
                && let Some(entry) = map.get_mut(&key)
            {
                entry.loads.insert(photo_id, state);
            }
            if let Some(bridge) = bridge.upgrade() {
                bridge.notify_ui();
            }
        });
    if spawned.is_err() {
        with_entry(live, |e| e.loads.insert(photo_id, LoadState::Failed));
        return VideoUserpicFetch::Failed;
    }
    VideoUserpicFetch::Pending
}

/// Picks the video size to play in the list: the small `u` loop, else the large `v` one.
fn pick_video_size(sizes: &[tl_enums::VideoSize]) -> Option<&tl_types::VideoSize> {
    let plain = || {
        sizes.iter().filter_map(|v| match v {
            tl_enums::VideoSize::VideoSize(vs) => Some(vs),
            _ => None,
        })
    };
    plain()
        .find(|vs| vs.r#type == "u")
        .or_else(|| plain().find(|vs| vs.r#type == "v"))
}

/// Downloads the video avatar bytes of `photo_id` for `user`.
fn load_video_userpic<C: RpcClient + Clone + 'static>(
    rpc: &C,
    handle: &Handle,
    user: tl_enums::InputUser,
    photo_id: i64,
) -> Result<Vec<u8>, String> {
    if matches!(user, tl_enums::InputUser::Empty) {
        return Err("video userpic owner is not a known user".to_owned());
    }
    let client = rpc.clone();
    let full = block_on_runtime(handle, async move {
        client
            .invoke(&tl_fns::users::GetFullUser { id: user })
            .await
            .map_err(Into::<tg_api::rpc::InvocationError>::into)
    })
    .map_err(|e| e.to_string())?;
    let tl_enums::users::UserFull::UserFull(resp) = full;
    let tl_enums::UserFull::UserFull(full_user) = resp.full_user;
    let photo = [
        full_user.personal_photo,
        full_user.profile_photo,
        full_user.fallback_photo,
    ]
    .into_iter()
    .flatten()
    .find_map(|p| match p {
        tl_enums::Photo::Photo(p) if p.id == photo_id => Some(p),
        _ => None,
    })
    .ok_or_else(|| format!("profile photo {photo_id} not found in users.getFullUser"))?;
    let sizes = photo.video_sizes.clone().unwrap_or_default();
    let size = pick_video_size(&sizes)
        .ok_or_else(|| format!("profile photo {photo_id} has no video size"))?;
    let location =
        tl_enums::InputFileLocation::InputPhotoFileLocation(tl_types::InputPhotoFileLocation {
            id: photo.id,
            access_hash: photo.access_hash,
            file_reference: photo.file_reference.clone(),
            thumb_size: size.r#type.clone(),
        });
    download_file_bytes(
        rpc,
        handle,
        photo.dc_id,
        location,
        u64::try_from(size.size).ok().filter(|&s| s > 0),
    )
    .map_err(|e| e.to_string())
}
