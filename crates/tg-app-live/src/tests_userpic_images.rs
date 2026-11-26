//! Live bug (alpha.2): chat list and chats showed placeholder circles because no widget ever
//! asked for a peer's photo. The live source must download the small profile photo with
//! `upload.getFile(inputPeerPhotoFileLocation{big: false})` and hand back the decoded image.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tg_api::{DcId, InvocationError, RpcClient};
use tg_data::{PeerId as DataPeerId, UserId as DataUserId};
use tg_session::{SessionRuntime, SessionRuntimeConfig};
use tg_tl::{
    Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums, functions as tl_fns,
    types as tl_types,
};
use tg_ui::userpic_images::UserpicImageSource;
use tokio::sync::broadcast;

use super::LiveUserpicSource;
use crate::LiveSession;

const SELF_UID: i64 = 810_001;
const PEER_UID: i64 = 810_002;
const PHOTO_ID: i64 = 5_550_001;

#[derive(Clone)]
struct UserpicFakeRpc {
    file_requests: Arc<Mutex<Vec<tl_fns::upload::GetFile>>>,
    png: Arc<Vec<u8>>,
    updates_tx: broadcast::Sender<tl_enums::Updates>,
}

impl RpcClient for UserpicFakeRpc {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        self.invoke_in_dc(2, req).await
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        _dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let bytes = req.to_bytes();
        let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let raw = if ctor == tl_fns::upload::GetFile::CONSTRUCTOR_ID {
            let parsed = tl_fns::upload::GetFile::from_bytes(&bytes)
                .map_err(|e| InvocationError::Deserialize(e.to_string()))?;
            let offset = usize::try_from(parsed.offset).unwrap_or(0);
            let chunk = self.png.get(offset..).unwrap_or_default().to_vec();
            self.file_requests.lock().expect("lock").push(parsed);
            tl_enums::upload::File::File(tl_types::upload::File {
                r#type: tl_enums::storage::FileType::FileJpeg,
                mtime: 0,
                bytes: chunk,
            })
            .to_bytes()
        } else if ctor == tl_fns::updates::GetState::CONSTRUCTOR_ID {
            tl_enums::updates::State::State(tl_types::updates::State {
                pts: 10,
                qts: 0,
                date: 1_750_000_000,
                seq: 1,
                unread_count: 0,
            })
            .to_bytes()
        } else {
            return Err(InvocationError::Dropped);
        };
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        2
    }

    fn set_main_dc_id(&self, _dc_id: DcId) {}

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        Some(self.updates_tx.subscribe())
    }
}

fn png_bytes() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(160, 160, image::Rgba([200, 40, 40, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)
        .expect("encode png");
    out.into_inner()
}

fn peer_user_with_photo() -> tl_enums::User {
    tl_enums::User::User(tl_types::User {
        self_: false,
        contact: true,
        mutual_contact: false,
        deleted: false,
        bot: false,
        bot_chat_history: false,
        bot_nochats: false,
        verified: false,
        restricted: false,
        min: false,
        bot_inline_geo: false,
        support: false,
        scam: false,
        apply_min_photo: false,
        fake: false,
        bot_attach_menu: false,
        premium: false,
        attach_menu_enabled: false,
        bot_can_edit: false,
        close_friend: false,
        stories_hidden: false,
        stories_unavailable: false,
        contact_require_premium: false,
        bot_business: false,
        bot_has_main_app: false,
        bot_forum_view: false,
        bot_forum_can_manage_topics: false,
        bot_can_manage_bots: false,
        bot_guestchat: false,
        bot_guard: false,
        bot_forum_sidebar: false,
        id: PEER_UID,
        access_hash: Some(PEER_UID * 100 + 3),
        first_name: Some("PhotoPeer".to_owned()),
        last_name: None,
        username: None,
        phone: None,
        photo: Some(tl_enums::UserProfilePhoto::UserProfilePhoto(
            tl_types::UserProfilePhoto {
                has_video: false,
                personal: false,
                photo_id: PHOTO_ID,
                stripped_thumb: None,
                dc_id: 4,
            },
        )),
        status: None,
        bot_info_version: None,
        restriction_reason: None,
        bot_inline_placeholder: None,
        lang_code: None,
        emoji_status: None,
        usernames: None,
        stories_max_id: None,
        color: None,
        profile_color: None,
        bot_active_users: None,
        bot_verification_icon: None,
        send_paid_messages_stars: None,
        linked_community_id: None,
    })
}

#[test]
fn live_userpic_source_downloads_and_decodes_the_small_peer_photo() {
    let dir = TempDir::new().expect("tempdir");
    let (updates_tx, _) = broadcast::channel(8);
    let fake = UserpicFakeRpc {
        file_requests: Arc::new(Mutex::new(Vec::new())),
        png: Arc::new(png_bytes()),
        updates_tx,
    };
    let cfg = SessionRuntimeConfig {
        init_updates_state: true,
        run_get_difference_on_start: false,
        load_initial_dialogs: false,
        fetch_self_user_on_start: false,
        ..SessionRuntimeConfig::default()
    };
    let runtime = Arc::new(
        SessionRuntime::start(
            DataUserId(SELF_UID as u64),
            crate::ErasedRpcClient::new(fake.clone()),
            cfg,
        )
        .expect("start runtime"),
    );
    let live = LiveSession::new(Arc::clone(&runtime), 7);
    live.set_media_cache_dir(dir.path().join("media_cache"))
        .expect("media cache dir");
    let session = Arc::clone(runtime.session());
    runtime.tokio_handle().block_on(async move {
        let mut guard = session.write().await;
        tg_api::convert::apply_tl_users_and_chats(&mut guard, &[peer_user_with_photo()], &[]);
    });

    let source = LiveUserpicSource::new(&live);
    let peer = DataPeerId::from_user(DataUserId(PEER_UID as u64)).0;
    let deadline = Instant::now() + Duration::from_secs(10);
    let image = loop {
        if let Some(image) = source.userpic(peer) {
            break image;
        }
        assert!(Instant::now() < deadline, "userpic never loaded");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(image.photo_id, PHOTO_ID);
    assert_eq!((image.width, image.height), (160, 160));
    assert_eq!(
        image.rgba[3], 255,
        "the photo stays square: the painter cuts the circle at the drawn size"
    );
    assert_eq!(
        image.rgba[(80 * 160 + 80) * 4],
        200,
        "center keeps the photo"
    );

    let requests = fake.file_requests.lock().expect("lock").clone();
    assert!(!requests.is_empty());
    match &requests[0].location {
        tl_enums::InputFileLocation::InputPeerPhotoFileLocation(loc) => {
            assert!(!loc.big, "the list uses the small photo");
            assert_eq!(loc.photo_id, PHOTO_ID);
            assert!(matches!(
                loc.peer,
                tl_enums::InputPeer::User(ref u) if u.user_id == PEER_UID
            ));
        }
        other => panic!("unexpected location {other:?}"),
    }
    // A second call is served from the decoded cache without another request.
    let before = requests.len();
    assert!(source.userpic(peer).is_some());
    assert_eq!(fake.file_requests.lock().expect("lock").len(), before);
    assert!(
        source
            .userpic(DataPeerId::from_user(DataUserId(1)).0)
            .is_none()
    );
}
