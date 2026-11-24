//! Modal dialog boxes hosted on `tg-ui`'s [`LayerStack`] and [`BoxModal`] system.
//!
//! Port origin: `Telegram/SourceFiles/boxes/`, `Telegram/SourceFiles/boxes/peers/`,
//! and `Telegram/SourceFiles/ui/boxes/`.

pub mod about;
pub mod about_cocoon;
pub mod add_contact;
pub mod add_proxy;
pub mod bot_info_extras;
pub mod calendar;
pub mod chat_import;
pub mod click_handlers;
pub mod collectible_info;
pub mod community;
pub mod compose_ai;
pub mod confirm;
pub mod confirm_phone;
pub mod create_peer;
pub mod delete_messages;
pub use tg_app_core::core_types::dto::dictionaries;
pub mod edit_peer;
pub mod emoji_userpic;
pub mod invite_links;
pub mod join_invite;
pub mod join_requests;
pub mod language;
pub mod managed_bot;
pub mod moderate_messages;
pub mod ownership_transfer;
pub mod peer_color;
pub mod peer_list;
pub mod peer_qr;
pub mod peer_reactions;
pub mod peer_tags;
pub mod photo_crop;
pub mod report;
pub mod self_destruct;
pub mod share;
pub mod sticker_creator;
pub mod sticker_set;
pub mod strings;
pub mod terms;
pub mod username;
pub mod verify_peers;
pub mod welcome_messages;

use egui::{Align2, Area, CornerRadius, CursorIcon, Id, Key, Order, Pos2, Rect, Sense, Ui, Vec2};
use tg_lang::tr;
use tg_ui::anim::{SimpleAnimation, Transition, interpolate_f32, with_alpha};
use tg_ui::fonts::font_id_from_style;
use tg_ui::icons::paint_style_icon;
use tg_ui::layers::{BoxButtonKind, BoxModal};
use tg_ui::painter::{BoxShadow, RoundRect};
use tg_ui::stand_in::{BoxStyle, Colors, ScrollAreaStyle};
use tg_ui::tg_style::st;
use tg_ui::theme::ColorsResolver;
use tg_ui::widgets::buttons::RoundButton;
use tg_ui::widgets::scroll_area::TgScrollArea;

use crate::boxes::about::AboutBoxState;
use crate::boxes::about_cocoon::AboutCocoonBoxState;
use crate::boxes::add_contact::AddContactBoxState;
use crate::boxes::add_proxy::{AddProxyBoxState, route_proxy_link};
use crate::boxes::bot_info_extras::{
    BotIntroPreviewBoxState, MusicAttachBoxState, TranslateBotInfoBoxState,
};
use crate::boxes::calendar::{CalendarBoxAction, CalendarBoxState};
use crate::boxes::chat_import::{ChatImportBoxAction, ChatImportBoxState, ChatImportStep};
use crate::boxes::click_handlers::{
    BankCardActionBoxState, BankCardBodyAction, PhoneActionBodyAction, PhoneActionBoxState,
    UrlAuthConfirmBoxState,
};
use crate::boxes::collectible_info::{CollectibleInfoBodyAction, CollectibleInfoBoxState};
use crate::boxes::community::{
    AddToCommunityBoxState, CommunityBoxAction, CommunityBoxState,
    CommunityPendingRequestsBoxState, CommunityRequestsBoxAction, CreateCommunityBoxState,
    ManageCommunityAction, ManageCommunityBoxState,
};
use crate::boxes::compose_ai::{AiComposeTone, PreviewAiToneBoxState};
use crate::boxes::confirm::{
    ConfirmBoxState, ConfirmPhoneBodyAction, ConfirmPhoneCodeBoxState, ConfirmTag,
};
use crate::boxes::create_peer::{
    CreateChannelBoxState, CreateChannelStep, CreateGroupBoxState, CreateGroupStep,
    CreatePeerBodyAction,
};
use crate::boxes::delete_messages::DeleteMessagesBoxState;
use crate::boxes::edit_peer::{
    AdminsListAction, AdminsListBoxState, AutoDeleteTimerBoxState, EditAdminRightsBoxState,
    EditPeerInfoBoxState, EditPeerSubAction, EditPermissionsBoxState,
};
use crate::boxes::emoji_userpic::EmojiUserpicBoxState;
use crate::boxes::invite_links::{EditInviteLinksBoxState, InviteLinksBoxAction, InviteLinksView};
use crate::boxes::join_invite::JoinInviteBoxState;
use crate::boxes::join_requests::{JoinRequestsAction, JoinRequestsBoxState};
use crate::boxes::language::LanguageBoxState;
use crate::boxes::managed_bot::{CreateManagedBotBoxState, EditBotInfoBoxState};
use crate::boxes::moderate_messages::ModerateMessagesBoxState;
use crate::boxes::ownership_transfer::{OwnershipTransferBoxState, OwnershipTransferStep};
use crate::boxes::peer_color::EditPeerColorBoxState;
use crate::boxes::peer_list::{
    PeerListBoxState, PeerListPurpose, PeerSelectionMode, filter_switch_inline_peers,
};
use crate::boxes::peer_qr::{PeerQrBodyAction, PeerQrBoxState};
use crate::boxes::peer_reactions::EditPeerReactionsBoxState;
use crate::boxes::peer_tags::EditPeerTagBoxState;
use crate::boxes::photo_crop::PhotoCropBoxState;
use crate::boxes::report::{ReportBoxState, ReportStep};
use crate::boxes::share::ShareBoxState;
use crate::boxes::sticker_set::{StickerSetBoxState, StickerSetOwnerAction};
use crate::boxes::terms::TermsOfServiceBoxState;
use crate::boxes::username::{UsernameBoxState, UsernameListAction};
use crate::boxes::verify_peers::{VerifyPeersAction, VerifyPeersBoxState};
use crate::boxes::welcome_messages::{WelcomeMessagesAction, WelcomeMessagesBoxState};
use crate::info::backend::InfoBackend;
use tg_app_core::core_types::window_types::DeepLinkOverlayRequest;
use tg_app_core::stand_in::types::{MsgId, PeerId};

/// Every modal box variant managed by [`BoxHost`].
#[derive(Debug)]
pub enum BoxKind {
    /// Generic confirmation dialog (`Ui::ConfirmBox`).
    Confirm(ConfirmBoxState),
    /// Phone confirmation verification code dialog (`Ui::ConfirmPhoneBox` / `tg://confirmphone`).
    ConfirmPhoneCode(ConfirmPhoneCodeBoxState),
    /// Add Contact dialog (`AddContactBox`).
    AddContact(AddContactBoxState),
    /// 2-step Create Group wizard (`GroupInfoBox` -> `AddParticipantsBox`).
    CreateGroup(CreateGroupBoxState),
    /// 2-step Create Channel wizard (`GroupInfoBox` -> `SetupChannelBox`).
    CreateChannel(CreateChannelBoxState),
    /// Edit Peer Info dialog (`EditPeerInfoBox`).
    EditPeerInfo(EditPeerInfoBoxState),
    /// Group Permissions + Slow Mode editor (`EditPeerPermissionsBox`).
    EditPermissions(EditPermissionsBoxState),
    /// Group/Channel Administrators list.
    AdminsList(AdminsListBoxState),
    /// Single Administrator rights + custom title editor (`EditAdminBox`).
    EditAdminRights(EditAdminRightsBoxState),
    /// Auto-Delete Timer selector (`TTLBox`).
    AutoDeleteTimer(AutoDeleteTimerBoxState),
    /// Generic searchable Peer List picker (`PeerListBox`).
    PeerList(PeerListBoxState),
    /// Share / Forward to multiple chats (`ShareBox`).
    Share(ShareBoxState),
    /// Report peer or messages (`ReportMessagesBox`).
    Report(ReportBoxState),
    /// Delete messages / leave chat (`DeleteMessagesBox`).
    DeleteMessages(DeleteMessagesBoxState),
    /// Edit public username (`UsernameBox`).
    Username(UsernameBoxState),
    /// Photo Crop editor (`tg-editor` crop math).
    PhotoCrop {
        /// Crop state.
        state: Box<PhotoCropBoxState>,
        /// Optional peer whose photo is being updated.
        target_peer: Option<PeerId>,
    },
    /// UI Language selector (`LanguageBox`).
    Language(LanguageBoxState),
    /// About Rustgram dialog (`AboutBox`).
    About(AboutBoxState),
    /// About Cocoon confidential-compute info dialog (`Ui::AboutCocoonBox`).
    AboutCocoon(AboutCocoonBoxState),
    /// Terms of Service dialog (`TermsBox`).
    TermsOfService(TermsOfServiceBoxState),
    /// Add Proxy confirmation dialog (`ProxiesBoxController::ShowApplyConfirmation`).
    AddProxy(AddProxyBoxState),
    /// Group / Channel invite preview dialog (`GroupInviteBox` / `tg://join?invite=...`).
    JoinInvite(JoinInviteBoxState),
    /// Invite Links manager, editor, and stats dialog (`EditInviteLinksBox`).
    EditInviteLinks(EditInviteLinksBoxState),
    /// Pending Join Requests approval dialog (`JoinRequestsBox`).
    JoinRequests(JoinRequestsBoxState),
    /// Peer accent & profile color + emoji background editor (`EditPeerColorBox`).
    EditPeerColor(EditPeerColorBoxState),
    /// Group / Channel ownership transfer dialog with 2FA SRP check (`OwnershipTransferBox`).
    OwnershipTransfer(OwnershipTransferBoxState),
    /// External chat history import wizard (WhatsApp / LINE / KakaoTalk).
    ChatImport(ChatImportBoxState),
    /// Jump-to-date chat calendar modal (`CalendarBox` / `messages.getSearchResultsCalendar`).
    Calendar(CalendarBoxState),
    /// Profile QR card (`PeerQrBox`).
    PeerQr(PeerQrBoxState),
    /// Emoji Userpic Builder (`info/userpic/*`).
    EmojiUserpic(EmojiUserpicBoxState),
    /// Per-chat allowed reactions editor (`EditPeerReactionsBox`).
    EditPeerReactions(EditPeerReactionsBoxState),
    /// Bot custom peer verification dialog (`VerifyPeersBox`).
    VerifyPeers(VerifyPeersBoxState),
    /// Sticker or custom-emoji pack preview dialog (`StickerSetBox` / `tg://addstickers`).
    StickerSet(StickerSetBoxState),
    /// Community overview dialog (`CommunityBox`).
    Community(CommunityBoxState),
    /// Create Community dialog (`communities.create`).
    CreateCommunity(CreateCommunityBoxState),
    /// Manage Community settings dialog (`ManageCommunityBox`).
    ManageCommunity(ManageCommunityBoxState),
    /// Community pending peer link requests dialog (`CommunityPendingRequestsBox`).
    CommunityPendingRequests(CommunityPendingRequestsBoxState),
    /// Add chat/channel/bot to a community dialog (`AddToCommunityBox`).
    AddToCommunity(AddToCommunityBoxState),
    /// Create Managed Bot dialog (`CreateManagedBotBox` / `tg://newbot`).
    CreateManagedBot(CreateManagedBotBoxState),
    /// Edit Managed Bot Info localization dialog (`EditBotInfoBox`).
    EditBotInfo(EditBotInfoBoxState),
    /// Group message moderation dialog (`ModerateMessagesBox`).
    ModerateMessages(ModerateMessagesBoxState),
    /// Group Welcome Messages editor dialog (`WelcomeMessagesBox`).
    WelcomeMessages(WelcomeMessagesBoxState),
    /// Shared AI Compose style preview dialog (`PreviewAiToneBox` / `tg://addstyle`).
    PreviewAiTone(PreviewAiToneBoxState),
    /// Telegram Login / OAuth URL authorization dialog (`UrlAuthBox` / `tg://oauth`).
    UrlAuth(UrlAuthConfirmBoxState),
    /// Phone number click action dialog (`core/phone_click_handler.cpp`).
    PhoneAction(PhoneActionBoxState),
    /// Bank card click action dialog (`core/bank_card_click_handler.cpp`).
    BankCardAction(BankCardActionBoxState),
    /// Fragment Collectible username / anonymous phone info dialog (`Ui::CollectibleInfoBox`).
    CollectibleInfo(CollectibleInfoBoxState),
    /// Peer member custom rank / tag editor dialog (`EditTagControl` / `EditCustomRankBox`).
    EditPeerTag(EditPeerTagBoxState),
    /// Standalone Bot Intro chat bubble preview dialog (`boxes/peers/bot_intro_preview.cpp`).
    BotIntroPreview(BotIntroPreviewBoxState),
    /// Multi-language Bot Info translation progress dialog (`boxes/peers/translate_bot_info_box.cpp`).
    TranslateBotInfo(TranslateBotInfoBoxState),
    /// Saved profile music attachment picker dialog (`boxes/music_attach_box.cpp`).
    MusicAttach(MusicAttachBoxState),
}

impl BoxKind {
    /// Builds the corresponding `tg-ui` [`BoxModal`] descriptor for `id`.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        match self {
            Self::Confirm(s) => s.to_modal(id),
            Self::ConfirmPhoneCode(s) => s.to_modal(id),
            Self::AddContact(s) => s.to_modal(id),
            Self::CreateGroup(s) => s.to_modal(id),
            Self::CreateChannel(s) => s.to_modal(id),
            Self::EditPeerInfo(s) => s.to_modal(id),
            Self::EditPermissions(s) => s.to_modal(id),
            Self::AdminsList(s) => s.to_modal(id),
            Self::EditAdminRights(s) => s.to_modal(id),
            Self::AutoDeleteTimer(s) => s.to_modal(id),
            Self::PeerList(s) => s.to_modal(id),
            Self::Share(s) => s.to_modal(id),
            Self::Report(s) => s.to_modal(id),
            Self::DeleteMessages(s) => s.to_modal(id),
            Self::Username(s) => s.to_modal(id),
            Self::PhotoCrop { state, .. } => state.to_modal(id),
            Self::Language(s) => s.to_modal(id),
            Self::About(s) => s.to_modal(id),
            Self::AboutCocoon(s) => s.to_modal(id),
            Self::TermsOfService(s) => s.to_modal(id),
            Self::AddProxy(s) => s.to_modal(id),
            Self::JoinInvite(s) => s.to_modal(id),
            Self::EditInviteLinks(s) => s.to_modal(id),
            Self::JoinRequests(s) => s.to_modal(id),
            Self::EditPeerColor(s) => s.to_modal(id),
            Self::OwnershipTransfer(s) => s.to_modal(id),
            Self::ChatImport(s) => s.to_modal(id),
            Self::Calendar(s) => s.to_modal(id),
            Self::PeerQr(s) => s.to_modal(id),
            Self::EmojiUserpic(s) => s.to_modal(id),
            Self::EditPeerReactions(s) => s.to_modal(id),
            Self::VerifyPeers(s) => s.to_modal(id),
            Self::StickerSet(s) => s.to_modal(id),
            Self::Community(s) => s.to_modal(id),
            Self::CreateCommunity(s) => s.to_modal(id),
            Self::ManageCommunity(s) => s.to_modal(id),
            Self::CommunityPendingRequests(s) => s.to_modal(id),
            Self::AddToCommunity(s) => s.to_modal(id),
            Self::CreateManagedBot(s) => s.to_modal(id),
            Self::EditBotInfo(s) => s.to_modal(id),
            Self::BotIntroPreview(s) => s.to_modal(id),
            Self::TranslateBotInfo(s) => s.to_modal(id),
            Self::MusicAttach(s) => s.to_modal(id),
            Self::ModerateMessages(s) => s.to_modal(id),
            Self::WelcomeMessages(s) => s.to_modal(id),
            Self::PreviewAiTone(s) => s.to_modal(id),
            Self::UrlAuth(s) => s.to_modal(id),
            Self::PhoneAction(s) => s.to_modal(id),
            Self::BankCardAction(s) => s.to_modal(id),
            Self::CollectibleInfo(s) => s.to_modal(id),
            Self::EditPeerTag(s) => s.to_modal(id),
        }
    }
}

/// High-level outcomes emitted by [`BoxHost::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoxEvent {
    /// Display a transient toast message.
    Toast(String),
    /// A new peer (contact, group, or channel) was created and should be opened.
    PeerCreated(PeerId),
    /// A peer's profile or settings were updated.
    PeerUpdated(PeerId),
    /// A photo crop completed with final pixel dimensions `(width, height)`.
    PhotoCropped {
        /// Optional target peer id.
        target_peer: Option<PeerId>,
        /// Output width in pixels.
        width: u32,
        /// Output height in pixels.
        height: u32,
    },
    /// Confirmation dialog accepted.
    Confirmed(ConfirmTag, bool),
    /// Messages were forwarded to `count` chats.
    Forwarded {
        /// Number of target chats.
        target_count: usize,
    },
    /// User confirmed forwarding a fullscreen media viewer item to `targets` via
    /// [`tg_app_core::media::backend::MediaBackend::forward_item_to`].
    ForwardMediaItem {
        /// Media viewer item ID.
        item_id: tg_app_core::media::backend::MediaItemId,
        /// Selected destination peers.
        targets: Vec<PeerId>,
        /// Optional accompanying comment.
        comment: String,
    },
    /// Messages were deleted or peer was left.
    Deleted {
        /// Target peer id.
        peer_id: PeerId,
        /// Number of deleted messages.
        count: usize,
        /// Whether deleted for everyone.
        revoked: bool,
    },
    /// Language pack switched.
    LanguageChanged(String),
    /// Terms of Service accepted or declined.
    TermsResolved {
        /// `true` if accepted, `false` if declined.
        accepted: bool,
    },
    /// User chose a target chat from the `switch_inline` peer picker box.
    SwitchInlinePeerChosen {
        /// Selected destination chat peer id.
        target_peer: PeerId,
        /// Target bot peer id (`PeerId::from_user(...)`).
        bot_id: PeerId,
        /// Target bot username without leading `@`.
        bot_username: String,
        /// Pre-filled inline query text.
        query: String,
    },
    /// User confirmed joining a chat/channel from [`BoxKind::JoinInvite`].
    JoinInviteConfirmed {
        /// Invite hash to pass to `messages.importChatInvite`.
        hash: String,
    },
    /// External chat history import completed (`messages.startHistoryImport`).
    ChatHistoryImported {
        /// Destination peer ID.
        peer_id: PeerId,
        /// Server-assigned import ID.
        import_id: i64,
        /// Count of uploaded media attachments.
        uploaded_media_count: usize,
    },
    /// User picked a date in [`BoxKind::Calendar`] to jump to in chat history.
    CalendarJumpToMessage {
        /// Target chat peer id.
        peer_id: PeerId,
        /// Target message id (`MsgId(0)` if date-only).
        msg_id: MsgId,
        /// Selected UTC Unix timestamp.
        date_unix: i32,
    },
    /// User requested opening a higher-layer overlay host via a `tg-app-core` request
    /// (e.g. [`DeepLinkOverlayRequest::OpenShareTheme`], keeping the crate DAG intact).
    OverlayRequest(DeepLinkOverlayRequest),
    /// User clicked Install or Remove inside [`BoxKind::StickerSet`].
    StickerSetToggleInstall {
        /// Numeric sticker set ID.
        set_id: u64,
        /// Short name slug.
        short_name: String,
        /// Pack display title.
        title: String,
        /// `true` for custom emoji packs, `false` for sticker packs.
        is_emoji: bool,
        /// `true` to install, `false` to remove.
        install: bool,
    },
    /// Pack owner triggered a rename, reorder, sticker removal, or pack deletion inside
    /// [`BoxKind::StickerSet`] (`sticker_set_box.cpp`).
    StickerSetOwnerAction(StickerSetOwnerAction),
    /// User clicked Add Style or Remove Style inside [`BoxKind::PreviewAiTone`].
    AiToneToggleSave {
        /// Tone being saved or removed.
        tone: AiComposeTone,
        /// `false` to add/save, `true` to remove/unsave.
        unsave: bool,
    },
    /// User clicked Next Example inside [`BoxKind::PreviewAiTone`].
    AiToneNextExample,
    /// User authorized or declined an OAuth URL login request inside [`BoxKind::UrlAuth`].
    UrlAuthResolved {
        /// Original URL auth request data.
        data: Box<tg_bots::callback::UrlAuthConfirmationBox>,
        /// User's accept/decline decision and optional permissions.
        choice: tg_bots::callback::UrlAuthUserChoice,
    },
    /// User clicked View Profile inside [`BoxKind::PhoneAction`].
    OpenPeerProfile(PeerId),
    /// User clicked an external link (e.g. bank app URL inside [`BoxKind::BankCardAction`]).
    OpenExternalUrl(String),
    /// User selected one or more saved music tracks to send inside [`BoxKind::MusicAttach`].
    SendMusicTracks {
        /// Target chat peer.
        peer_id: PeerId,
        /// Selected saved audio tracks.
        tracks: Vec<crate::info::backend::SavedMusicTrack>,
    },
    /// User submitted a verification code inside [`BoxKind::ConfirmPhoneCode`] (`account.confirmPhone`).
    ConfirmPhoneSubmitted {
        /// Display phone number from the deep link.
        phone: String,
        /// `phone_code_hash` returned by `account.sendConfirmPhoneCode`.
        phone_code_hash: String,
        /// Digits-only verification code entered by the user.
        code: String,
    },
}

struct HostedBox {
    id: String,
    kind: BoxKind,
    enter_anim: SimpleAnimation,
    opened_frame: u64,
}

/// Synchronized host rendering all `tg-app` modal boxes with `tg-ui`'s [`BoxModal`],
/// [`BoxStyle`], [`BoxShadow`], [`TgScrollArea`], and fade/slide transitions.
#[derive(Default)]
pub struct BoxHost {
    stack: Vec<HostedBox>,
    backdrop_anim: SimpleAnimation,
    was_shown: bool,
    next_id: u64,
}

impl BoxHost {
    /// Creates an empty [`BoxHost`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` when no boxes are currently open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    /// Returns the number of stacked modal boxes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.stack.len()
    }

    /// Returns a reference to the topmost active [`BoxKind`], if any.
    #[must_use]
    pub fn top_kind(&self) -> Option<&BoxKind> {
        self.stack.last().map(|entry| &entry.kind)
    }

    /// Returns a mutable reference to the topmost active [`BoxKind`], if any.
    pub fn top_kind_mut(&mut self) -> Option<&mut BoxKind> {
        self.stack.last_mut().map(|entry| &mut entry.kind)
    }

    /// Returns `true` while any modal backdrop or box entrance animation is running.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.backdrop_anim.animating()
            || self.stack.iter().any(|entry| entry.enter_anim.animating())
    }

    /// Pushes a new [`BoxKind`] onto the modal stack with tdesktop's `boxDuration` animation.
    pub fn push(&mut self, ctx: &egui::Context, kind: BoxKind, colors: &dyn Colors) {
        self.next_id += 1;
        let id = format!("tg_box_{}", self.next_id);
        let style = BoxStyle::default_box(colors);
        let now = ctx.input(|i| i.time);
        let mut enter_anim = SimpleAnimation::new();
        enter_anim.start(now, 0.0, 1.0, style.duration_ms, Transition::EaseOutCirc);
        if self.stack.is_empty() {
            self.backdrop_anim
                .change(now, 1.0, style.duration_ms, Transition::EaseOutCirc);
            self.was_shown = true;
        }
        self.stack.push(HostedBox {
            id,
            kind,
            enter_anim,
            opened_frame: ctx.cumulative_frame_nr(),
        });
        ctx.request_repaint();
    }

    /// Pushes a new [`BoxKind`] onto the modal stack without requiring an active `egui::Context`
    /// (used by deep-link routing invoked outside a frame pass).
    pub fn push_immediate(&mut self, kind: BoxKind, _colors: &dyn Colors) {
        self.next_id += 1;
        let id = format!("tg_box_{}", self.next_id);
        let mut enter_anim = SimpleAnimation::new();
        enter_anim.start(0.0, 1.0, 1.0, 0, Transition::Linear);
        if self.stack.is_empty() {
            self.backdrop_anim
                .start(0.0, 1.0, 1.0, 0, Transition::Linear);
            self.was_shown = true;
        }
        self.stack.push(HostedBox {
            id,
            kind,
            enter_anim,
            opened_frame: 0,
        });
    }

    /// Pops the topmost box from the modal stack without requiring an active `egui::Context`
    /// (parallel to [`Self::push_immediate`]).
    pub fn pop_immediate(&mut self) {
        self.stack.pop();
        if self.stack.is_empty() {
            self.was_shown = false;
        }
    }

    /// Pops the topmost box from the modal stack.
    pub fn pop(&mut self, ctx: &egui::Context, colors: &dyn Colors) {
        let style = BoxStyle::default_box(colors);
        self.stack.pop();
        if self.stack.is_empty() && self.was_shown {
            let now = ctx.input(|i| i.time);
            self.was_shown = false;
            self.backdrop_anim
                .change(now, 0.0, style.duration_ms, Transition::Linear);
            ctx.request_repaint();
        }
    }

    /// Refreshes the topmost box when a wizard advances steps.
    fn refresh_top_modal(&mut self, ctx: &egui::Context, _colors: &dyn Colors) {
        if let Some(top) = self.stack.last_mut() {
            top.opened_frame = ctx.cumulative_frame_nr();
        }
        ctx.request_repaint();
    }

    /// Renders active modal boxes over `bounds`, dispatches button actions to `backend`,
    /// and returns any resulting [`BoxEvent`]s.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        bounds: Rect,
        backend: &mut dyn InfoBackend,
        colors: &dyn Colors,
    ) -> Vec<BoxEvent> {
        let mut events = Vec::new();
        let style = BoxStyle::default_box(colors);
        let now = ctx.input(|i| i.time);
        let backdrop_opacity = self
            .backdrop_anim
            .sample(now, if self.stack.is_empty() { 0.0 } else { 1.0 });
        if self.backdrop_anim.animating() {
            ctx.request_repaint();
        }
        if self.stack.is_empty() && backdrop_opacity <= 0.01 {
            return events;
        }

        // Full-bounds dimmed scrim (`layerBg`).
        let mut outside_clicked = false;
        Area::new(Id::unique("tg_boxhost_backdrop"))
            .order(Order::Middle)
            .fixed_pos(bounds.min)
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(bounds.size(), Sense::click());
                ui.painter().rect_filled(
                    rect,
                    CornerRadius::ZERO,
                    with_alpha(style.layer_bg, backdrop_opacity),
                );
                if resp.clicked() {
                    outside_clicked = true;
                }
            });

        if self.stack.is_empty() {
            return events;
        }

        let escape_pressed = ctx.input(|i| i.key_pressed(Key::Escape));
        let current_frame = ctx.cumulative_frame_nr();
        let contacts = backend.contacts();
        let all_peers = backend.all_peers();
        let languages = backend.languages();

        let mut deferred_push: Option<BoxKind> = None;
        let mut language_changed: Option<String> = None;
        let mut clicked_link: Option<String> = None;
        let mut triggered_action: Option<(String, String)> = None;
        let mut top_box_rect = Rect::NOTHING;
        let mut body_events: Vec<BoxEvent> = Vec::new();
        let mut body_refresh_modal = false;
        let mut body_should_pop = false;

        let top_idx = self.stack.len() - 1;
        for (idx, entry) in self.stack.iter_mut().enumerate() {
            let is_top = idx == top_idx;
            let progress = entry.enter_anim.sample(now, 1.0);
            if entry.enter_anim.animating() {
                ctx.request_repaint();
            }

            let modal = entry.kind.to_modal(&entry.id);
            let box_w = modal.width.min((bounds.width() - 32.0).max(260.0));
            let max_body_h = modal
                .max_content_height
                .max(420.0)
                .min((bounds.height() - 160.0).max(200.0));
            let slide_y = interpolate_f32(12.0, 0.0, progress as f32);
            let opacity = progress as f32;

            let area_id = Id::unique(("tg_boxhost_card", idx, &entry.id));
            Area::new(area_id)
                .order(Order::Foreground)
                .interactable(is_top)
                .anchor(Align2::CENTER_CENTER, Vec2::new(0.0, slide_y))
                .show(ctx, |ui| {
                    let frame_start = ui.cursor().min;
                    let bg_idx = ui.painter().add(egui::Shape::Noop);
                    let shadow_painter = ui.painter().clone();

                    let inner_resp = ui
                        .allocate_ui_with_layout(
                            Vec2::new(box_w, 0.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.set_min_width(box_w);
                                ui.set_max_width(box_w);

                                // Title row (`st::box_title_height`, `st::box_title_font`, `st::box_title_close_icon`).
                                let title_h = st::box_title_height.0 as f32;
                                let (title_rect, _) = ui
                                    .allocate_exact_size(Vec2::new(box_w, title_h), Sense::hover());
                                let title_font = font_id_from_style(st::box_title_font);
                                let title_fg = with_alpha(style.title.text_fg, f64::from(opacity));
                                let title_galley = ui.painter().layout_no_wrap(
                                    modal.title.clone(),
                                    title_font,
                                    title_fg,
                                );
                                let title_pos = Pos2::new(
                                    title_rect.min.x + st::box_title_position.x.0 as f32,
                                    title_rect.center().y - title_galley.size().y * 0.5,
                                );
                                ui.painter()
                                    .galley(title_pos, title_galley.clone(), title_fg);

                                if let Some(add) = &modal.additional_title {
                                    ui.painter().text(
                                        Pos2::new(
                                            title_pos.x
                                                + title_galley.size().x
                                                + st::box_title_additional_skip.0 as f32,
                                            title_rect.center().y,
                                        ),
                                        Align2::LEFT_CENTER,
                                        add,
                                        font_id_from_style(st::box_title_additional_font),
                                        with_alpha(style.title_additional_fg, f64::from(opacity)),
                                    );
                                }

                                if modal.show_close_button {
                                    let close_w = st::box_title_close.width.0 as f32;
                                    let close_h = st::box_title_close.height.0 as f32;
                                    let close_rect = Rect::from_min_size(
                                        Pos2::new(
                                            title_rect.max.x - close_w,
                                            title_rect.center().y - close_h * 0.5,
                                        ),
                                        Vec2::new(close_w, close_h),
                                    );
                                    let c_resp = ui.interact(
                                        close_rect,
                                        ui.make_persistent_id((&entry.id, "box_title_close")),
                                        Sense::click(),
                                    );
                                    if c_resp.hovered() {
                                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                    }
                                    let close_icon = if c_resp.hovered() {
                                        st::box_title_close_icon_over
                                    } else {
                                        st::box_title_close_icon
                                    };
                                    let ic_rect = Rect::from_center_size(
                                        close_rect.center(),
                                        Vec2::splat(18.0),
                                    );
                                    paint_style_icon(
                                        ui.painter(),
                                        ic_rect,
                                        close_icon,
                                        &ColorsResolver(colors),
                                        None,
                                    );
                                    if c_resp.clicked() && is_top {
                                        triggered_action =
                                            Some((entry.id.clone(), "__close__".to_owned()));
                                    }
                                }

                                // Body content with generous natural height up to `max_body_h`.
                                let scroll_st = ScrollAreaStyle::solid(colors);
                                let body_w = (box_w - style.content_padding.horizontal()).max(80.0);
                                let uses_scroll = matches!(
                                    entry.kind,
                                    BoxKind::PeerList(_)
                                        | BoxKind::CreateGroup(CreateGroupBoxState {
                                            step: CreateGroupStep::AddMembers,
                                            ..
                                        })
                                        | BoxKind::EditPeerInfo(_)
                                        | BoxKind::EditPermissions(_)
                                        | BoxKind::AdminsList(_)
                                        | BoxKind::EditAdminRights(_)
                                        | BoxKind::AutoDeleteTimer(_)
                                        | BoxKind::Share(_)
                                        | BoxKind::Language(_)
                                        | BoxKind::TermsOfService(_)
                                        | BoxKind::EditInviteLinks(_)
                                        | BoxKind::JoinRequests(_)
                                        | BoxKind::EditPeerColor(_)
                                        | BoxKind::ChatImport(_)
                                        | BoxKind::PeerQr(_)
                                        | BoxKind::EmojiUserpic(_)
                                        | BoxKind::EditPeerReactions(_)
                                        | BoxKind::VerifyPeers(_)
                                        | BoxKind::StickerSet(_)
                                        | BoxKind::Community(_)
                                        | BoxKind::ManageCommunity(_)
                                        | BoxKind::CommunityPendingRequests(_)
                                        | BoxKind::AddToCommunity(_)
                                        | BoxKind::EditBotInfo(_)
                                        | BoxKind::BotIntroPreview(_)
                                        | BoxKind::TranslateBotInfo(_)
                                        | BoxKind::MusicAttach(_)
                                        | BoxKind::ModerateMessages(_)
                                        | BoxKind::WelcomeMessages(_)
                                        | BoxKind::CollectibleInfo(_)
                                        | BoxKind::EditPeerTag(_)
                                );
                                ui.horizontal(|ui| {
                                    ui.add_space(style.content_padding.left);
                                    ui.vertical(|ui| {
                                        ui.set_min_width(body_w);
                                        ui.set_max_width(body_w);
                                        if uses_scroll {
                                            ui.allocate_ui_with_layout(
                                                Vec2::new(body_w, max_body_h),
                                                egui::Layout::top_down(egui::Align::Min),
                                                |ui| {
                                                    TgScrollArea::new(&entry.id, scroll_st)
                                                        .max_height(max_body_h)
                                                        .show_shadows(true)
                                                        .show(ui, |ui| {
                                                            render_box_kind_body(
                                                                &mut entry.kind,
                                                                &entry.id,
                                                                ui,
                                                                backend,
                                                                &contacts,
                                                                &all_peers,
                                                                &languages,
                                                                colors,
                                                                &mut deferred_push,
                                                                &mut language_changed,
                                                                &mut clicked_link,
                                                                &mut body_events,
                                                                &mut body_refresh_modal,
                                                                &mut body_should_pop,
                                                            );
                                                        });
                                                },
                                            );
                                        } else {
                                            render_box_kind_body(
                                                &mut entry.kind,
                                                &entry.id,
                                                ui,
                                                backend,
                                                &contacts,
                                                &all_peers,
                                                &languages,
                                                colors,
                                                &mut deferred_push,
                                                &mut language_changed,
                                                &mut clicked_link,
                                                &mut body_events,
                                                &mut body_refresh_modal,
                                                &mut body_should_pop,
                                            );
                                        }
                                    });
                                });

                                ui.add_space(style.content_padding.bottom);

                                // Footer button row (`defaultBoxButton` / `attentionBoxButton`).
                                if !modal.buttons.is_empty() || modal.left_button.is_some() {
                                    ui.add_space(style.button_padding.top);
                                    ui.horizontal(|ui| {
                                        ui.set_min_width(box_w);
                                        ui.add_space(14.0);
                                        if let Some(lb) = &modal.left_button {
                                            let bst = match lb.kind {
                                                BoxButtonKind::Default => style.button,
                                                BoxButtonKind::Attention => style.attention_button,
                                            };
                                            if RoundButton::new(&lb.label, bst).show(ui).clicked()
                                                && is_top
                                            {
                                                triggered_action =
                                                    Some((entry.id.clone(), lb.id.clone()));
                                            }
                                        }

                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.add_space(style.button_padding.right);
                                                for btn in modal.buttons.iter().rev() {
                                                    let bst = match btn.kind {
                                                        BoxButtonKind::Default => style.button,
                                                        BoxButtonKind::Attention => {
                                                            style.attention_button
                                                        }
                                                    };
                                                    if RoundButton::new(&btn.label, bst)
                                                        .show(ui)
                                                        .clicked()
                                                        && is_top
                                                    {
                                                        triggered_action = Some((
                                                            entry.id.clone(),
                                                            btn.id.clone(),
                                                        ));
                                                    }
                                                    ui.add_space(style.button_padding.left);
                                                }
                                            },
                                        );
                                    });
                                    ui.add_space(style.button_padding.bottom);
                                }
                            },
                        )
                        .response;

                    let final_rect = Rect::from_min_size(
                        frame_start,
                        Vec2::new(box_w, inner_resp.rect.height()),
                    );
                    if is_top {
                        top_box_rect = final_rect;
                    }

                    let bg_shape = egui::Shape::rect_filled(
                        final_rect,
                        CornerRadius::same(style.radius.round() as u8),
                        with_alpha(style.bg, f64::from(opacity)),
                    );
                    ui.painter().set(bg_idx, bg_shape);

                    BoxShadow::new(style.shadow).paint_with_opacity(
                        ctx,
                        &shadow_painter,
                        final_rect,
                        style.radius,
                        style.shadow.opacity * opacity * 0.45,
                    );
                    if !is_top {
                        RoundRect::new(style.radius, with_alpha(style.layer_bg, 0.35))
                            .paint(ui.painter(), final_rect);
                    }
                });
        }

        if triggered_action.is_none()
            && let Some(top) = self.stack.last()
            && current_frame > top.opened_frame
        {
            let should_close = escape_pressed
                || (outside_clicked
                    && ctx
                        .pointer_interact_pos()
                        .is_none_or(|p| !top_box_rect.contains(p)));
            if should_close {
                triggered_action = Some((top.id.clone(), "__close__".to_owned()));
            }
        }

        if let Some(lang_id) = language_changed
            && backend.set_language(&lang_id).is_ok()
        {
            events.push(BoxEvent::LanguageChanged(lang_id.clone()));
            events.push(BoxEvent::Toast(format!("Language switched to '{lang_id}'")));
        }

        if let Some(url) = clicked_link {
            if let Some(proxy_box) = route_proxy_link(&url) {
                deferred_push = Some(proxy_box);
            } else {
                events.push(BoxEvent::Toast(format!("Link: {url}")));
            }
        }

        events.extend(body_events);
        if body_should_pop {
            self.pop(ctx, colors);
        } else if body_refresh_modal {
            self.refresh_top_modal(ctx, colors);
        }

        if let Some((box_id, action_id)) = triggered_action {
            self.handle_layer_event(
                ctx,
                &box_id,
                &action_id,
                backend,
                colors,
                &mut events,
                &mut deferred_push,
            );
        }

        if let Some(next_box) = deferred_push {
            self.push(ctx, next_box, colors);
        }

        events
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_layer_event(
        &mut self,
        ctx: &egui::Context,
        box_id: &str,
        action_id: &str,
        backend: &mut dyn InfoBackend,
        colors: &dyn Colors,
        events: &mut Vec<BoxEvent>,
        deferred_push: &mut Option<BoxKind>,
    ) {
        if action_id == "__close__" || action_id == "cancel" || action_id == "close" {
            if let Some(top) = self.stack.last()
                && top.id == box_id
                && let BoxKind::UrlAuth(s) = &top.kind
            {
                events.push(BoxEvent::UrlAuthResolved {
                    data: Box::new(s.data.clone()),
                    choice: s.to_user_choice(false),
                });
            }
            self.pop(ctx, colors);
            return;
        }

        let Some(top) = self.stack.last_mut() else {
            return;
        };
        if top.id != box_id {
            return;
        }

        let mut should_pop = false;
        let mut should_refresh_modal = false;
        let mut mark_parent_photo = false;

        match &mut top.kind {
            BoxKind::Confirm(s) if action_id == "confirm" => {
                let tag = s.tag.clone();
                let checked = s.checkbox_checked;
                if matches!(tag, ConfirmTag::DeclineTerms) {
                    events.push(BoxEvent::TermsResolved { accepted: false });
                    events.push(BoxEvent::Toast(strings::TOAST_TOS_DECLINED.to_owned()));
                }
                events.push(BoxEvent::Confirmed(tag, checked));
                should_pop = true;
            }
            BoxKind::ConfirmPhoneCode(s) if action_id == "send_code" => {
                if let Ok(code) = s.validate_code() {
                    s.submitting = true;
                    events.push(BoxEvent::ConfirmPhoneSubmitted {
                        phone: s.phone.clone(),
                        phone_code_hash: s.phone_code_hash.clone(),
                        code,
                    });
                }
            }
            BoxKind::AddContact(s) if action_id == "create" => {
                if let Ok(new_id) = s.submit(backend) {
                    events.push(BoxEvent::PeerCreated(new_id));
                    events.push(BoxEvent::Toast(strings::TOAST_CONTACT_ADDED.to_owned()));
                    should_pop = true;
                }
            }
            BoxKind::CreateGroup(s) => match (s.step, action_id) {
                (CreateGroupStep::Details, "next") => {
                    if s.advance_to_members().is_ok() {
                        should_refresh_modal = true;
                    }
                }
                (CreateGroupStep::AddMembers, "back") => {
                    s.back_to_details();
                    should_refresh_modal = true;
                }
                (CreateGroupStep::AddMembers, "create") => {
                    if let Ok(gid) = s.submit(backend) {
                        events.push(BoxEvent::PeerCreated(gid));
                        events.push(BoxEvent::Toast(strings::TOAST_GROUP_CREATED.to_owned()));
                        should_pop = true;
                    }
                }
                _ => {}
            },
            BoxKind::CreateChannel(s) => match (s.step, action_id) {
                (CreateChannelStep::Details, "next") => {
                    if s.advance_to_type().is_ok() {
                        should_refresh_modal = true;
                    }
                }
                (CreateChannelStep::ChannelType, "back") => {
                    s.back_to_details();
                    should_refresh_modal = true;
                }
                (CreateChannelStep::ChannelType, "save") => {
                    if let Ok(cid) = s.submit(backend) {
                        events.push(BoxEvent::PeerCreated(cid));
                        events.push(BoxEvent::Toast(strings::TOAST_CHANNEL_CREATED.to_owned()));
                        should_pop = true;
                    }
                }
                _ => {}
            },
            BoxKind::EditPeerInfo(s) if action_id == "save" => {
                let pid = s.peer_id;
                if s.submit(backend).is_ok() {
                    events.push(BoxEvent::PeerUpdated(pid));
                    events.push(BoxEvent::Toast(strings::TOAST_SAVED_CHANGES.to_owned()));
                    should_pop = true;
                }
            }
            BoxKind::EditPermissions(s) if action_id == "save" => {
                let pid = s.peer_id;
                if s.submit(backend).is_ok() {
                    events.push(BoxEvent::PeerUpdated(pid));
                    events.push(BoxEvent::Toast(
                        strings::TOAST_PERMISSIONS_UPDATED.to_owned(),
                    ));
                    should_pop = true;
                }
            }
            BoxKind::AdminsList(s) if action_id == "add_admin" => {
                *deferred_push = Some(BoxKind::PeerList(PeerListBoxState::new(
                    strings::ADD_ADMIN_TITLE,
                    strings::CONTINUE_BTN,
                    PeerSelectionMode::Single,
                    PeerListPurpose::PickAdmin { peer_id: s.peer_id },
                )));
            }
            BoxKind::EditAdminRights(s) => {
                let pid = s.peer_id;
                if action_id == "save" && s.submit(backend).is_ok() {
                    events.push(BoxEvent::PeerUpdated(pid));
                    events.push(BoxEvent::Toast(strings::TOAST_ADMIN_UPDATED.to_owned()));
                    should_pop = true;
                } else if action_id == "dismiss" && s.dismiss_admin(backend).is_ok() {
                    events.push(BoxEvent::PeerUpdated(pid));
                    events.push(BoxEvent::Toast(strings::TOAST_ADMIN_DISMISSED.to_owned()));
                    should_pop = true;
                }
            }
            BoxKind::EditPeerTag(s) => {
                let pid = s.peer_id;
                if action_id == "save"
                    && let Ok(saved) = s.submit(backend)
                {
                    events.push(BoxEvent::PeerUpdated(pid));
                    let msg = if saved.is_empty() {
                        "Member tag removed".to_owned()
                    } else {
                        format!("Member tag updated: {saved}")
                    };
                    events.push(BoxEvent::Toast(msg));
                    should_pop = true;
                }
            }
            BoxKind::AutoDeleteTimer(s) => {
                let pid = s.peer_id;
                if action_id == "disable" {
                    s.selected_ttl = 0;
                }
                if (action_id == "save" || action_id == "disable") && s.submit(backend).is_ok() {
                    events.push(BoxEvent::PeerUpdated(pid));
                    events.push(BoxEvent::Toast(strings::TOAST_TTL_UPDATED.to_owned()));
                    should_pop = true;
                }
            }
            BoxKind::PeerList(s) if action_id == "confirm" => match s.purpose.clone() {
                PeerListPurpose::AddMembers { group_id } => {
                    if let Ok(added) = backend.add_members(group_id, &s.selected) {
                        events.push(BoxEvent::PeerUpdated(group_id));
                        events.push(BoxEvent::Toast(format!("Added {added} member(s)")));
                        should_pop = true;
                    }
                }
                PeerListPurpose::LinkDiscussion { channel_id } => {
                    let chosen = s.selected.first().copied();
                    if backend.set_linked_discussion(channel_id, chosen).is_ok() {
                        events.push(BoxEvent::PeerUpdated(channel_id));
                        events.push(BoxEvent::Toast(
                            strings::TOAST_DISCUSSION_UPDATED.to_owned(),
                        ));
                        should_pop = true;
                    }
                }
                PeerListPurpose::PickAdmin { peer_id } => {
                    if let Some(&uid) = s.selected.first()
                        && let Some(peer) = backend.peer_profile(peer_id)
                        && let Some(member) = backend
                            .peer_members(peer_id, "")
                            .into_iter()
                            .find(|m| m.peer_id == uid)
                    {
                        *deferred_push = Some(BoxKind::EditAdminRights(
                            EditAdminRightsBoxState::from_member(&peer, &member),
                        ));
                        should_pop = true;
                    }
                }
                PeerListPurpose::SwitchInline {
                    bot_id,
                    bot_username,
                    query,
                    ..
                } => {
                    if let Some(&target_peer) = s.selected.first() {
                        events.push(BoxEvent::SwitchInlinePeerChosen {
                            target_peer,
                            bot_id,
                            bot_username,
                            query,
                        });
                        should_pop = true;
                    }
                }
                PeerListPurpose::Generic => {
                    should_pop = true;
                }
            },
            BoxKind::Share(s) => {
                if action_id == "copy_link" {
                    if let Some(ref link) = s.share_link {
                        events.push(BoxEvent::Toast(format!(
                            "{} {link}",
                            tr::lng_username_copied()
                        )));
                    }
                } else if action_id == "send" {
                    if let Some(item_id) = s.media_item_id {
                        if s.selected.is_empty() {
                            s.error = Some(strings::SHARE_SELECT_ONE_ERROR.to_owned());
                        } else {
                            s.error = None;
                            events.push(BoxEvent::ForwardMediaItem {
                                item_id,
                                targets: s.selected.clone(),
                                comment: s.comment.clone(),
                            });
                            should_pop = true;
                        }
                    } else if let Ok(n) = s.submit(backend) {
                        events.push(BoxEvent::Forwarded { target_count: n });
                        events.push(BoxEvent::Toast(
                            tr::lng_share_messages_to_many_chats(n as i64).replace("**", ""),
                        ));
                        should_pop = true;
                    }
                }
            }
            BoxKind::Report(s) => match (s.step, action_id) {
                (ReportStep::SelectReason, "next") => {
                    s.next_step();
                    should_refresh_modal = true;
                }
                (ReportStep::Details, "back") => {
                    s.prev_step();
                    should_refresh_modal = true;
                }
                (ReportStep::Details, "report") if s.submit(backend).is_ok() => {
                    events.push(BoxEvent::Toast(tr::lng_report_thanks()));
                    should_pop = true;
                }
                _ => {}
            },
            BoxKind::DeleteMessages(s) if action_id == "delete" => {
                if let Ok(count) = s.submit(backend) {
                    events.push(BoxEvent::Deleted {
                        peer_id: s.peer_id,
                        count,
                        revoked: s.revoke_for_everyone,
                    });
                    events.push(BoxEvent::Toast(strings::TOAST_DELETED.to_owned()));
                    should_pop = true;
                }
            }
            BoxKind::Username(s) if action_id == "save" => {
                let pid = s.peer_id;
                if s.submit(backend).is_ok() {
                    events.push(BoxEvent::PeerUpdated(pid));
                    events.push(BoxEvent::Toast(strings::TOAST_USERNAME_SAVED.to_owned()));
                    should_pop = true;
                }
            }
            BoxKind::PhotoCrop { state, target_peer } => {
                if action_id == "reset" {
                    state.reset();
                } else if action_id == "save" {
                    let tp = *target_peer;
                    match state.finalize() {
                        Ok(img) => {
                            events.push(BoxEvent::PhotoCropped {
                                target_peer: tp,
                                width: img.width(),
                                height: img.height(),
                            });
                            events.push(BoxEvent::Toast(format!(
                                "Photo cropped ({}×{})",
                                img.width(),
                                img.height()
                            )));
                            mark_parent_photo = true;
                            should_pop = true;
                        }
                        Err(err) => {
                            state.error = Some(err.to_string());
                        }
                    }
                }
            }
            BoxKind::TermsOfService(s) => {
                if action_id == "accept" {
                    if s.accept(backend).is_ok() {
                        events.push(BoxEvent::TermsResolved { accepted: true });
                        events.push(BoxEvent::Toast(strings::TOAST_TOS_ACCEPTED.to_owned()));
                        should_pop = true;
                    }
                } else if action_id == "decline" {
                    *deferred_push = Some(BoxKind::Confirm(
                        ConfirmBoxState::new(
                            strings::TOS_DECLINE_TITLE,
                            strings::TOS_DECLINE_BODY,
                            tr::lng_terms_decline(),
                            ConfirmTag::DeclineTerms,
                        )
                        .attention(true),
                    ));
                }
            }
            BoxKind::AddProxy(s) if action_id == "connect" && s.is_valid() => {
                events.push(BoxEvent::Confirmed(
                    ConfirmTag::ApplyProxy(s.proxy.clone()),
                    true,
                ));
                events.push(BoxEvent::Toast(format!(
                    "Connecting to proxy {}:{}...",
                    s.proxy.host, s.proxy.port
                )));
                should_pop = true;
            }
            BoxKind::JoinInvite(s) if action_id == "join" => {
                events.push(BoxEvent::JoinInviteConfirmed {
                    hash: s.hash.clone(),
                });
                should_pop = true;
            }
            BoxKind::EditInviteLinks(s) => match action_id {
                "back_list" => {
                    s.view = InviteLinksView::List;
                    s.error = None;
                    should_refresh_modal = true;
                }
                "save_link" => {
                    let pid = s.peer_id;
                    if s.submit_editor(backend).is_ok() {
                        events.push(BoxEvent::PeerUpdated(pid));
                        events.push(BoxEvent::Toast("Invite link saved".to_owned()));
                        should_refresh_modal = true;
                    }
                }
                "edit_stats_link" => {
                    if let InviteLinksView::Stats { ref link, .. } = s.view.clone() {
                        s.open_edit_form(link);
                        should_refresh_modal = true;
                    }
                }
                _ => {}
            },
            BoxKind::JoinRequests(s) => {
                let pid = s.peer_id;
                if action_id == "approve_all" {
                    if let Ok(n) = s.decide_all(true, backend) {
                        events.push(BoxEvent::PeerUpdated(pid));
                        events.push(BoxEvent::Toast(format!("Approved {n} join request(s)")));
                    }
                } else if action_id == "dismiss_all"
                    && let Ok(n) = s.decide_all(false, backend)
                {
                    events.push(BoxEvent::PeerUpdated(pid));
                    events.push(BoxEvent::Toast(format!("Dismissed {n} join request(s)")));
                }
            }
            BoxKind::EditPeerColor(s) => {
                let pid = s.peer_id;
                if action_id == "reset_profile_color" {
                    s.profile_color_index = None;
                    s.profile_background_emoji_id = 0;
                    s.custom_emoji_input.clear();
                    if s.submit(backend).is_ok() {
                        events.push(BoxEvent::PeerUpdated(pid));
                        events.push(BoxEvent::Toast("Profile color reset".to_owned()));
                        should_pop = true;
                    }
                } else if action_id == "apply_color" && s.submit(backend).is_ok() {
                    events.push(BoxEvent::PeerUpdated(pid));
                    events.push(BoxEvent::Toast("Appearance updated".to_owned()));
                    should_pop = true;
                }
            }
            BoxKind::OwnershipTransfer(s) => {
                let pid = s.peer_id;
                if action_id == "transfer_confirm" {
                    s.proceed_to_password();
                    should_refresh_modal = true;
                } else if action_id == "transfer_submit" && s.submit_password(backend).is_ok() {
                    events.push(BoxEvent::PeerUpdated(pid));
                    events.push(BoxEvent::Toast(format!(
                        "Ownership transferred to {}",
                        s.new_owner_name
                    )));
                    if s.step == OwnershipTransferStep::Completed {
                        should_refresh_modal = true;
                    }
                }
            }
            BoxKind::ChatImport(s) => match action_id {
                "import_back" => {
                    s.error = None;
                    s.step = match s.step {
                        ChatImportStep::Confirm => ChatImportStep::SelectPeer,
                        _ => ChatImportStep::SelectFile,
                    };
                    should_refresh_modal = true;
                }
                "import_start" => {
                    if let Ok(summary) = s.start_import(backend) {
                        events.push(BoxEvent::ChatHistoryImported {
                            peer_id: summary.peer_id,
                            import_id: summary.import_id,
                            uploaded_media_count: summary.uploaded_media_count,
                        });
                        events.push(BoxEvent::Toast(format!(
                            "Chat history import started ({} media uploaded)",
                            summary.uploaded_media_count
                        )));
                        should_refresh_modal = true;
                    }
                }
                _ => {}
            },
            BoxKind::Calendar(s) if action_id == "jump" => {
                if let Some(CalendarBoxAction::JumpToDate {
                    peer_id,
                    msg_id,
                    date_unix,
                }) = s.build_jump_action()
                {
                    events.push(BoxEvent::CalendarJumpToMessage {
                        peer_id,
                        msg_id,
                        date_unix,
                    });
                    should_pop = true;
                }
            }
            BoxKind::PeerQr(s) => {
                if action_id == "copy_qr_link" {
                    let link = s.link.clone();
                    s.status_message = Some(format!("Link copied: {link}"));
                    events.push(BoxEvent::Toast(format!("QR link copied: {link}")));
                } else if action_id == "share_qr" {
                    *deferred_push = Some(BoxKind::Share(ShareBoxState::new(
                        Vec::new(),
                        Some(s.link.clone()),
                    )));
                }
            }
            BoxKind::CollectibleInfo(s) if action_id == "copy_collectible" => {
                let copy = s.info.copy_text.clone();
                s.status_message = Some(format!("Copied: {copy}"));
                events.push(BoxEvent::Toast(format!("Copied: {copy}")));
            }
            BoxKind::EmojiUserpic(s) if action_id == "save_emoji_userpic" => {
                let pid = s.peer_id;
                if s.submit(backend).is_ok() {
                    events.push(BoxEvent::PeerUpdated(pid));
                    events.push(BoxEvent::Toast("Emoji avatar updated".to_owned()));
                    mark_parent_photo = true;
                    should_pop = true;
                }
            }
            BoxKind::EditPeerReactions(s) if action_id == "save_reactions" => {
                let pid = s.peer_id;
                if s.submit(backend).is_ok() {
                    events.push(BoxEvent::PeerUpdated(pid));
                    events.push(BoxEvent::Toast("Chat reactions updated".to_owned()));
                    should_pop = true;
                }
            }
            BoxKind::VerifyPeers(s) => {
                if action_id == "verify_back" {
                    s.back_to_list();
                    should_refresh_modal = true;
                } else if action_id == "verify_submit"
                    && let Ok(msg) = s.submit_current(backend)
                {
                    if !msg.is_empty() {
                        events.push(BoxEvent::Toast(msg));
                    }
                    should_refresh_modal = true;
                }
            }
            BoxKind::StickerSet(s) => match action_id {
                "install_set" => {
                    s.installed = true;
                    s.archived = false;
                    events.push(BoxEvent::StickerSetToggleInstall {
                        set_id: s.set_id,
                        short_name: s.short_name.clone(),
                        title: s.title.clone(),
                        is_emoji: s.is_emoji,
                        install: true,
                    });
                    should_pop = true;
                }
                "remove_set" => {
                    s.installed = false;
                    events.push(BoxEvent::StickerSetToggleInstall {
                        set_id: s.set_id,
                        short_name: s.short_name.clone(),
                        title: s.title.clone(),
                        is_emoji: s.is_emoji,
                        install: false,
                    });
                    should_pop = true;
                }
                "done_reorder" => {
                    s.set_reorder_mode(false);
                    should_refresh_modal = true;
                }
                "copy_link" => {
                    events.push(BoxEvent::Toast(format!(
                        "{} {}",
                        tr::lng_username_copied(),
                        s.share_url()
                    )));
                }
                _ => {}
            },
            BoxKind::CreateCommunity(s) if action_id == "create" => {
                if let Ok(cid) = s.submit(backend) {
                    events.push(BoxEvent::PeerCreated(cid));
                    events.push(BoxEvent::Toast("Community created".to_owned()));
                    should_pop = true;
                }
            }
            BoxKind::ManageCommunity(s) if action_id == "save" => {
                let cid = s.community_id;
                if s.submit(backend).is_ok() {
                    events.push(BoxEvent::PeerUpdated(cid));
                    events.push(BoxEvent::Toast("Community settings saved".to_owned()));
                    should_pop = true;
                }
            }
            BoxKind::CommunityPendingRequests(s) => {
                let cid = s.community_id;
                if action_id == "approve_all" && s.decide_all(false, backend).is_ok() {
                    events.push(BoxEvent::PeerUpdated(cid));
                    events.push(BoxEvent::Toast("Approved all link requests".to_owned()));
                    should_refresh_modal = true;
                } else if action_id == "reject_all" && s.decide_all(true, backend).is_ok() {
                    events.push(BoxEvent::PeerUpdated(cid));
                    events.push(BoxEvent::Toast("Rejected all link requests".to_owned()));
                    should_refresh_modal = true;
                }
            }
            BoxKind::AddToCommunity(s) if action_id == "add" => {
                let cid = s.community_id;
                if let Ok(linked_now) = s.submit(backend) {
                    events.push(BoxEvent::PeerUpdated(cid));
                    events.push(BoxEvent::Toast(if linked_now {
                        "Chat added to community".to_owned()
                    } else {
                        "Request to add chat sent to community admins".to_owned()
                    }));
                    should_pop = true;
                }
            }
            BoxKind::CreateManagedBot(s) if action_id == "create_bot" => {
                if let Ok(bot_id) = s.submit(backend) {
                    events.push(BoxEvent::PeerCreated(bot_id));
                    events.push(BoxEvent::Toast(format!(
                        "Managed bot @{} created",
                        s.full_username()
                    )));
                    should_pop = true;
                }
            }
            BoxKind::EditBotInfo(s) if action_id == "save_bot_info" => {
                let bid = s.bot_id;
                if let Ok(saved) = s.save_all(backend) {
                    events.push(BoxEvent::PeerUpdated(bid));
                    events.push(BoxEvent::Toast(if saved > 0 {
                        format!("Saved bot info for {saved} language(s)")
                    } else {
                        "No changes to save".to_owned()
                    }));
                    should_pop = true;
                }
            }
            BoxKind::TranslateBotInfo(s) => match action_id {
                "translate_start" => {
                    if let Ok(count) = s.start_translate(backend) {
                        events.push(BoxEvent::Toast(tr::lng_bot_languages_translated(
                            count as i64,
                        )));
                    }
                    should_refresh_modal = true;
                }
                "translate_stop" => {
                    s.stop();
                    should_refresh_modal = true;
                }
                "translate_save" => {
                    let bid = s.bot_id;
                    if let Ok(saved) = s.save_translations(backend) {
                        events.push(BoxEvent::PeerUpdated(bid));
                        events.push(BoxEvent::Toast(format!(
                            "Saved {saved} translated language(s)"
                        )));
                        should_pop = true;
                    }
                }
                _ => {}
            },
            BoxKind::MusicAttach(s) if action_id == "music_attach_send" => {
                let tracks = s.selected_tracks();
                if tracks.is_empty() {
                    s.error = Some("Select at least one track to send.".to_owned());
                } else {
                    let count = tracks.len();
                    let peer_id = s.target_peer_id;
                    events.push(BoxEvent::SendMusicTracks { peer_id, tracks });
                    events.push(BoxEvent::Toast(format!("Attached {count} track(s)")));
                    should_pop = true;
                }
            }
            BoxKind::ModerateMessages(s) if action_id == "moderate_confirm" => {
                let pid = s.peer_id;
                if let Ok(res) = s.submit(backend) {
                    events.push(BoxEvent::Deleted {
                        peer_id: pid,
                        count: res.deleted_selected,
                        revoked: true,
                    });
                    events.push(BoxEvent::Toast(format!(
                        "Moderated: {} deleted, {} reported, {} banned",
                        res.deleted_selected, res.reported_peers, res.banned_peers
                    )));
                    should_pop = true;
                }
            }
            BoxKind::PreviewAiTone(s) => match action_id {
                "next_example" => {
                    events.push(BoxEvent::AiToneNextExample);
                }
                "toggle_save_tone" => {
                    events.push(BoxEvent::AiToneToggleSave {
                        tone: s.tone.clone(),
                        unsave: s.installed,
                    });
                    should_pop = true;
                }
                _ => {}
            },
            BoxKind::UrlAuth(s) if action_id == "authorize" => {
                events.push(BoxEvent::UrlAuthResolved {
                    data: Box::new(s.data.clone()),
                    choice: s.to_user_choice(true),
                });
                should_pop = true;
            }
            BoxKind::PhoneAction(s) if action_id == "copy_phone" => {
                ctx.copy_text(s.phone.clone());
                events.push(BoxEvent::Toast(format!("Phone number copied: {}", s.phone)));
                should_pop = true;
            }
            BoxKind::BankCardAction(s) if action_id == "copy_card" => {
                ctx.copy_text(s.number.clone());
                events.push(BoxEvent::Toast(format!("Card number copied: {}", s.number)));
                should_pop = true;
            }
            _ => {}
        }

        if should_pop {
            self.pop(ctx, colors);
            if mark_parent_photo && let Some(parent) = self.stack.last_mut() {
                match &mut parent.kind {
                    BoxKind::CreateGroup(g) => g.has_custom_photo = true,
                    BoxKind::CreateChannel(c) => c.has_custom_photo = true,
                    BoxKind::EditPeerInfo(e) => e.has_custom_photo = true,
                    _ => {}
                }
            }
        } else if should_refresh_modal {
            self.refresh_top_modal(ctx, colors);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_box_kind_body(
    kind: &mut BoxKind,
    box_id: &str,
    ui: &mut Ui,
    backend: &mut dyn InfoBackend,
    contacts: &[crate::info::backend::PeerProfile],
    all_peers: &[crate::info::backend::PeerProfile],
    languages: &[crate::info::backend::LanguageEntry],
    colors: &dyn Colors,
    deferred_push: &mut Option<BoxKind>,
    language_changed: &mut Option<String>,
    clicked_link: &mut Option<String>,
    body_events: &mut Vec<BoxEvent>,
    body_refresh_modal: &mut bool,
    body_should_pop: &mut bool,
) {
    match kind {
        BoxKind::Confirm(s) => s.show_body(ui, colors),
        BoxKind::ConfirmPhoneCode(s) => {
            if let Some(act) = s.show_body(ui, colors) {
                match act {
                    ConfirmPhoneBodyAction::SubmitCode => {
                        if let Ok(code) = s.validate_code() {
                            s.submitting = true;
                            body_events.push(BoxEvent::ConfirmPhoneSubmitted {
                                phone: s.phone.clone(),
                                phone_code_hash: s.phone_code_hash.clone(),
                                code,
                            });
                        }
                    }
                    ConfirmPhoneBodyAction::OpenFragmentUrl(url) => {
                        body_events.push(BoxEvent::OpenExternalUrl(url));
                    }
                }
            }
        }
        BoxKind::AddContact(s) => s.show_body(ui, colors),
        BoxKind::CreateGroup(s) => {
            if let Some(CreatePeerBodyAction::OpenPhotoCrop) = s.show_body(ui, contacts, colors) {
                *deferred_push = Some(BoxKind::PhotoCrop {
                    state: Box::new(PhotoCropBoxState::sample_profile_photo(
                        strings::CROP_GROUP_PHOTO_TITLE,
                        [0x5C, 0xAF, 0xFA],
                    )),
                    target_peer: None,
                });
            }
        }
        BoxKind::CreateChannel(s) => {
            if let Some(CreatePeerBodyAction::OpenPhotoCrop) = s.show_body(ui, backend, colors) {
                *deferred_push = Some(BoxKind::PhotoCrop {
                    state: Box::new(PhotoCropBoxState::sample_profile_photo(
                        strings::CROP_CHANNEL_PHOTO_TITLE,
                        [0x9A, 0xD1, 0x64],
                    )),
                    target_peer: None,
                });
            }
        }
        BoxKind::EditPeerInfo(s) => {
            let peer_snap = backend.peer_profile(s.peer_id);
            if let Some(sub) = s.show_body(ui, peer_snap.as_ref(), backend, colors) {
                match sub {
                    EditPeerSubAction::OpenPhotoCrop => {
                        *deferred_push = Some(BoxKind::PhotoCrop {
                            state: Box::new(PhotoCropBoxState::sample_profile_photo(
                                strings::CROP_UPDATE_PHOTO_TITLE,
                                [0x5C, 0xAF, 0xFA],
                            )),
                            target_peer: Some(s.peer_id),
                        });
                    }
                    EditPeerSubAction::OpenPermissions => {
                        if let Some(p) = peer_snap {
                            *deferred_push = Some(BoxKind::EditPermissions(
                                EditPermissionsBoxState::from_profile(&p),
                            ));
                        }
                    }
                    EditPeerSubAction::OpenAdministrators => {
                        *deferred_push =
                            Some(BoxKind::AdminsList(AdminsListBoxState::new(s.peer_id)));
                    }
                    EditPeerSubAction::OpenInviteLinks => {
                        let title = peer_snap
                            .as_ref()
                            .map(|p| p.title.clone())
                            .unwrap_or_else(|| s.title.clone());
                        *deferred_push = Some(BoxKind::EditInviteLinks(
                            EditInviteLinksBoxState::new(s.peer_id, title),
                        ));
                    }
                    EditPeerSubAction::OpenJoinRequests => {
                        let is_group = peer_snap
                            .as_ref()
                            .map_or(s.kind.is_group(), |p| p.kind.is_group());
                        *deferred_push = Some(BoxKind::JoinRequests(JoinRequestsBoxState::new(
                            s.peer_id, is_group,
                        )));
                    }
                    EditPeerSubAction::OpenPeerColor => {
                        let title = peer_snap
                            .as_ref()
                            .map(|p| p.title.clone())
                            .unwrap_or_else(|| s.title.clone());
                        let settings = backend.peer_color_settings(s.peer_id);
                        *deferred_push = Some(BoxKind::EditPeerColor(EditPeerColorBoxState::new(
                            s.peer_id, title, settings,
                        )));
                    }
                    EditPeerSubAction::OpenEmojiUserpic => {
                        let title = peer_snap
                            .as_ref()
                            .map(|p| p.title.clone())
                            .unwrap_or_else(|| s.title.clone());
                        *deferred_push = Some(BoxKind::EmojiUserpic(EmojiUserpicBoxState::new(
                            s.peer_id, title,
                        )));
                    }
                    EditPeerSubAction::OpenReactions => {
                        let title = peer_snap
                            .as_ref()
                            .map(|p| p.title.clone())
                            .unwrap_or_else(|| s.title.clone());
                        let is_channel = peer_snap
                            .as_ref()
                            .map_or(s.kind.is_channel(), |p| p.kind.is_channel());
                        let cfg = backend.peer_available_reactions(s.peer_id);
                        *deferred_push = Some(BoxKind::EditPeerReactions(
                            EditPeerReactionsBoxState::new(s.peer_id, title, is_channel, cfg),
                        ));
                    }
                    EditPeerSubAction::OpenLinkedDiscussion => {
                        let current = peer_snap
                            .and_then(|p| p.linked_discussion_peer.map(|(id, _)| id))
                            .into_iter()
                            .collect();
                        *deferred_push = Some(BoxKind::PeerList(
                            PeerListBoxState::new(
                                strings::LINK_DISCUSSION_TITLE,
                                strings::LINK_CONFIRM_BTN,
                                PeerSelectionMode::Single,
                                PeerListPurpose::LinkDiscussion {
                                    channel_id: s.peer_id,
                                },
                            )
                            .with_selected(current),
                        ));
                    }
                    EditPeerSubAction::OpenAutoDelete => {
                        let ttl = peer_snap.map_or(0, |p| p.auto_delete_ttl_seconds);
                        *deferred_push = Some(BoxKind::AutoDeleteTimer(
                            AutoDeleteTimerBoxState::new(s.peer_id, ttl),
                        ));
                    }
                    EditPeerSubAction::OpenWelcomeMessages => {
                        let title = peer_snap
                            .as_ref()
                            .map(|p| p.title.clone())
                            .unwrap_or_else(|| s.title.clone());
                        *deferred_push = Some(BoxKind::WelcomeMessages(
                            WelcomeMessagesBoxState::new(s.peer_id, title),
                        ));
                    }
                    EditPeerSubAction::OpenCommunity => {
                        if let Some(comm) = backend.community_for_peer(s.peer_id) {
                            *deferred_push = Some(BoxKind::Community(CommunityBoxState::new(
                                comm.id, comm.title,
                            )));
                        } else {
                            let title = peer_snap
                                .as_ref()
                                .map(|p| p.title.clone())
                                .unwrap_or_else(|| s.title.clone());
                            *deferred_push = Some(BoxKind::CreateCommunity(
                                CreateCommunityBoxState::new(s.peer_id, title),
                            ));
                        }
                    }
                }
            }
        }
        BoxKind::EditPermissions(s) => s.show_body(ui, colors),
        BoxKind::AdminsList(s) => {
            let members = backend.peer_members(s.peer_id, "");
            if let Some(AdminsListAction::EditAdmin(uid)) = s.show_body(ui, &members, colors)
                && let Some(peer) = backend.peer_profile(s.peer_id)
                && let Some(member) = members.iter().find(|m| m.peer_id == uid)
            {
                *deferred_push = Some(BoxKind::EditAdminRights(
                    EditAdminRightsBoxState::from_member(&peer, member),
                ));
            }
        }
        BoxKind::EditAdminRights(s) => {
            s.show_body(ui, colors);
            if s.show_transfer_ownership_row(ui, colors) {
                let title = backend
                    .peer_profile(s.peer_id)
                    .map(|p| p.title)
                    .unwrap_or_else(|| "Chat".to_owned());
                *deferred_push = Some(BoxKind::OwnershipTransfer(OwnershipTransferBoxState::new(
                    s.peer_id,
                    title,
                    !s.is_group,
                    s.user_id,
                    s.user_name.clone(),
                )));
            }
        }
        BoxKind::EditPeerTag(s) => s.show_body(ui, colors),
        BoxKind::AutoDeleteTimer(s) => s.show_body(ui, colors),
        BoxKind::PeerList(s) => {
            let pool: Vec<_> = match s.purpose {
                PeerListPurpose::AddMembers { .. } => contacts.to_vec(),
                PeerListPurpose::LinkDiscussion { .. } => all_peers
                    .iter()
                    .filter(|p| p.kind.is_group())
                    .cloned()
                    .collect(),
                PeerListPurpose::PickAdmin { peer_id } => {
                    let members = backend.peer_members(peer_id, "");
                    members
                        .into_iter()
                        .filter_map(|m| backend.peer_profile(m.peer_id))
                        .collect()
                }
                PeerListPurpose::SwitchInline {
                    bot_id, peer_types, ..
                } => filter_switch_inline_peers(all_peers, bot_id, peer_types)
                    .into_iter()
                    .cloned()
                    .collect(),
                PeerListPurpose::Generic => all_peers.to_vec(),
            };
            s.show_body(ui, box_id, &pool, colors);
        }
        BoxKind::Share(s) => s.show_body(ui, all_peers, colors),
        BoxKind::Report(s) => s.show_body(ui, colors),
        BoxKind::DeleteMessages(s) => s.show_body(ui, colors),
        BoxKind::Username(s) => {
            if let Some(act) = s.show_body(ui, backend, colors) {
                match act {
                    UsernameListAction::CopiedLink(link) => {
                        body_events.push(BoxEvent::Toast(format!(
                            "{} {link}",
                            tr::lng_username_copied()
                        )));
                    }
                    UsernameListAction::Toggled { username, active } => {
                        body_events.push(BoxEvent::PeerUpdated(s.peer_id));
                        body_events.push(BoxEvent::Toast(format!(
                            "@{username} {}",
                            if active { "activated" } else { "hidden" }
                        )));
                    }
                    UsernameListAction::Reordered(_) => {
                        body_events.push(BoxEvent::PeerUpdated(s.peer_id));
                        body_events.push(BoxEvent::Toast("Usernames reordered".to_owned()));
                    }
                }
            }
        }
        BoxKind::PhotoCrop { state, .. } => state.show_body(ui, colors),
        BoxKind::Language(s) => {
            if let Some(new_id) = s.show_body(ui, languages, colors) {
                *language_changed = Some(new_id);
            }
        }
        BoxKind::About(s) => {
            if let Some(url) = s.show_body(ui, colors) {
                *clicked_link = Some(url);
            }
        }
        BoxKind::AboutCocoon(s) => {
            if let Some(url) = s.show_body(ui, colors, false) {
                *clicked_link = Some(url);
            }
        }
        BoxKind::TermsOfService(s) => s.show_body(ui, colors),
        BoxKind::AddProxy(s) => s.show_body(ui, colors),
        BoxKind::JoinInvite(s) => s.show_body(ui, colors),
        BoxKind::EditInviteLinks(s) => {
            if let Some(act) = s.show_body(ui, backend, colors) {
                match act {
                    InviteLinksBoxAction::Toast(msg) => body_events.push(BoxEvent::Toast(msg)),
                    InviteLinksBoxAction::RefreshModal => *body_refresh_modal = true,
                    InviteLinksBoxAction::LinksChanged => {
                        body_events.push(BoxEvent::PeerUpdated(s.peer_id));
                    }
                }
            }
        }
        BoxKind::JoinRequests(s) => {
            if let Some(act) = s.show_body(ui, backend, colors) {
                body_events.push(BoxEvent::PeerUpdated(s.peer_id));
                match act {
                    JoinRequestsAction::SingleResolved { approved, .. } => {
                        body_events.push(BoxEvent::Toast(if approved {
                            "Join request approved".to_owned()
                        } else {
                            "Join request dismissed".to_owned()
                        }));
                    }
                    JoinRequestsAction::AllResolved { count, approved } => {
                        body_events.push(BoxEvent::Toast(if approved {
                            format!("Approved {count} join request(s)")
                        } else {
                            format!("Dismissed {count} join request(s)")
                        }));
                    }
                }
                *body_refresh_modal = true;
            }
        }
        BoxKind::EditPeerColor(s) => s.show_body(ui, colors),
        BoxKind::OwnershipTransfer(s) => s.show_body(ui, backend, colors),
        BoxKind::ChatImport(s) => {
            if let Some(act) = s.show_body(ui, backend, all_peers, colors) {
                match act {
                    ChatImportBoxAction::RefreshModal => *body_refresh_modal = true,
                    ChatImportBoxAction::Completed(sum) => {
                        body_events.push(BoxEvent::ChatHistoryImported {
                            peer_id: sum.peer_id,
                            import_id: sum.import_id,
                            uploaded_media_count: sum.uploaded_media_count,
                        });
                        *body_refresh_modal = true;
                    }
                }
            }
        }
        BoxKind::PeerQr(s) => {
            if let Some(act) = s.show_body(ui, colors) {
                match act {
                    PeerQrBodyAction::CopyLink(link) => {
                        body_events.push(BoxEvent::Toast(format!("QR link copied: {link}")));
                    }
                    PeerQrBodyAction::ShareLink(link) => {
                        *deferred_push =
                            Some(BoxKind::Share(ShareBoxState::new(Vec::new(), Some(link))));
                    }
                    PeerQrBodyAction::OpenShareTheme {
                        peer_id,
                        peer_title,
                    } => {
                        body_events.push(BoxEvent::OverlayRequest(
                            DeepLinkOverlayRequest::OpenShareTheme {
                                peer_id,
                                peer_name: peer_title,
                            },
                        ));
                    }
                    PeerQrBodyAction::SavePng { filename, bytes } => {
                        body_events.push(BoxEvent::Toast(format!(
                            "Prepared {filename} ({} bytes)",
                            bytes.len()
                        )));
                    }
                }
            }
        }
        BoxKind::CollectibleInfo(s) => {
            if let Some(act) = s.show_body(ui, colors) {
                match act {
                    CollectibleInfoBodyAction::CopyEntity(text)
                    | CollectibleInfoBodyAction::CopyPrimary(text) => {
                        body_events.push(BoxEvent::Toast(format!("Copied: {text}")));
                    }
                    CollectibleInfoBodyAction::OpenFragmentUrl(url) => {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(url.clone()));
                        body_events.push(BoxEvent::Toast(format!("Opening Fragment: {url}")));
                    }
                }
            }
        }
        BoxKind::Calendar(s) => {
            if let Some(CalendarBoxAction::JumpToDate {
                peer_id,
                msg_id,
                date_unix,
            }) = s.show_box_body(ui, colors)
            {
                body_events.push(BoxEvent::CalendarJumpToMessage {
                    peer_id,
                    msg_id,
                    date_unix,
                });
            }
        }
        BoxKind::EmojiUserpic(s) => s.show_body(ui, colors),
        BoxKind::EditPeerReactions(s) => s.show_body(ui, colors),
        BoxKind::VerifyPeers(s) => {
            if let Some(VerifyPeersAction::RefreshModal) =
                s.show_body(ui, all_peers, backend, colors)
            {
                *body_refresh_modal = true;
            }
        }
        BoxKind::StickerSet(s) => {
            let prev_reorder = s.reorder_mode;
            let prev_title = s.title.clone();
            let prev_count = s.count;
            s.show_body(ui, colors);
            if s.reorder_mode != prev_reorder || s.title != prev_title || s.count != prev_count {
                *body_refresh_modal = true;
            }
            for act in s.take_owner_actions() {
                body_events.push(BoxEvent::StickerSetOwnerAction(act));
            }
        }
        BoxKind::Community(s) => {
            if let Some(act) = s.show_body(ui, backend, colors) {
                match act {
                    CommunityBoxAction::OpenPeer(pid) => {
                        body_events.push(BoxEvent::PeerCreated(pid));
                    }
                    CommunityBoxAction::OpenManage(cid) => {
                        if let Some(details) = backend.community_details(cid) {
                            *deferred_push = Some(BoxKind::ManageCommunity(
                                ManageCommunityBoxState::from_details(&details),
                            ));
                        }
                    }
                    CommunityBoxAction::OpenPendingRequests(cid) => {
                        *deferred_push = Some(BoxKind::CommunityPendingRequests(
                            CommunityPendingRequestsBoxState::new(cid),
                        ));
                    }
                    CommunityBoxAction::OpenAddChat(cid) => {
                        *deferred_push =
                            Some(BoxKind::AddToCommunity(AddToCommunityBoxState::new(cid)));
                    }
                    CommunityBoxAction::CollapsedToggled(collapsed) => {
                        body_events.push(BoxEvent::PeerUpdated(s.community_id));
                        body_events.push(BoxEvent::Toast(if collapsed {
                            "Community chats grouped into one row".to_owned()
                        } else {
                            "Community chats shown individually".to_owned()
                        }));
                    }
                    CommunityBoxAction::PeerRemoved(_) => {
                        body_events.push(BoxEvent::PeerUpdated(s.community_id));
                        body_events.push(BoxEvent::Toast("Chat removed from community".to_owned()));
                    }
                    CommunityBoxAction::Toast(msg) => body_events.push(BoxEvent::Toast(msg)),
                }
            }
        }
        BoxKind::CreateCommunity(s) => s.show_body(ui, colors),
        BoxKind::ManageCommunity(s) => {
            if let Some(act) = s.show_body(ui, backend, colors) {
                match act {
                    ManageCommunityAction::OpenAdministrators(cid) => {
                        *deferred_push = Some(BoxKind::AdminsList(AdminsListBoxState::new(cid)));
                    }
                    ManageCommunityAction::OpenPendingRequests(cid) => {
                        *deferred_push = Some(BoxKind::CommunityPendingRequests(
                            CommunityPendingRequestsBoxState::new(cid),
                        ));
                    }
                    ManageCommunityAction::OpenAddChat(cid) => {
                        *deferred_push =
                            Some(BoxKind::AddToCommunity(AddToCommunityBoxState::new(cid)));
                    }
                    ManageCommunityAction::Deleted(cid) => {
                        body_events.push(BoxEvent::PeerUpdated(cid));
                        body_events.push(BoxEvent::Toast("Community deleted".to_owned()));
                    }
                    ManageCommunityAction::PeerLinksChanged => {
                        body_events.push(BoxEvent::PeerUpdated(s.community_id));
                    }
                    ManageCommunityAction::Toast(msg) => body_events.push(BoxEvent::Toast(msg)),
                }
            }
        }
        BoxKind::CommunityPendingRequests(s) => {
            if let Some(act) = s.show_body(ui, backend, colors) {
                body_events.push(BoxEvent::PeerUpdated(s.community_id));
                match act {
                    CommunityRequestsBoxAction::SingleResolved { approved, .. } => {
                        body_events.push(BoxEvent::Toast(if approved {
                            "Link request approved".to_owned()
                        } else {
                            "Link request rejected".to_owned()
                        }));
                    }
                    CommunityRequestsBoxAction::AllResolved { approved } => {
                        body_events.push(BoxEvent::Toast(if approved {
                            "Approved all link requests".to_owned()
                        } else {
                            "Rejected all link requests".to_owned()
                        }));
                    }
                }
                *body_refresh_modal = true;
            }
        }
        BoxKind::AddToCommunity(s) => s.show_body(ui, all_peers, backend, colors),
        BoxKind::CreateManagedBot(s) => s.show_body(ui, backend, colors),
        BoxKind::EditBotInfo(s) => {
            if let Some(crate::boxes::managed_bot::EditBotInfoSubAction::OpenTranslateBox) =
                s.show_body(ui, backend, colors)
            {
                let initial_codes = if s.active_lang.is_empty() {
                    vec!["es".to_owned(), "de".to_owned()]
                } else {
                    vec![s.active_lang.clone()]
                };
                *deferred_push = Some(BoxKind::TranslateBotInfo(TranslateBotInfoBoxState::new(
                    s.bot_id,
                    s.bot_username.clone(),
                    initial_codes,
                    Some(s.draft.clone()),
                )));
            }
        }
        BoxKind::BotIntroPreview(s) => s.show_body(ui, colors),
        BoxKind::TranslateBotInfo(s) => s.show_body(ui, colors),
        BoxKind::MusicAttach(s) => s.show_body(ui, backend, colors),
        BoxKind::ModerateMessages(s) => s.show_body(ui, colors),
        BoxKind::WelcomeMessages(s) => {
            if let Some(act) = s.show_body(ui, backend, colors) {
                match act {
                    WelcomeMessagesAction::Changed => {
                        body_events.push(BoxEvent::PeerUpdated(s.section.peer_id));
                    }
                    WelcomeMessagesAction::Toast(msg) => body_events.push(BoxEvent::Toast(msg)),
                    WelcomeMessagesAction::Back => {}
                }
            }
        }
        BoxKind::PreviewAiTone(s) => s.show_box_body(ui, colors),
        BoxKind::UrlAuth(s) => s.show_body(ui, colors),
        BoxKind::PhoneAction(s) => {
            if let Some(act) = s.show_body(ui, colors) {
                match act {
                    PhoneActionBodyAction::CopyPhone(phone) => {
                        body_events.push(BoxEvent::Toast(format!("Phone number copied: {phone}")));
                        *body_should_pop = true;
                    }
                    PhoneActionBodyAction::AddToContacts {
                        first_name,
                        last_name,
                        phone,
                    } => {
                        *deferred_push = Some(BoxKind::AddContact(
                            AddContactBoxState::with_prefill(first_name, last_name, phone),
                        ));
                    }
                    PhoneActionBodyAction::ViewProfile(pid) => {
                        body_events.push(BoxEvent::OpenPeerProfile(pid));
                        *body_should_pop = true;
                    }
                }
            }
        }
        BoxKind::BankCardAction(s) => {
            if let Some(act) = s.show_body(ui, colors) {
                match act {
                    BankCardBodyAction::CopyNumber(number) => {
                        body_events.push(BoxEvent::Toast(format!("Card number copied: {number}")));
                        *body_should_pop = true;
                    }
                    BankCardBodyAction::OpenBankUrl(url) => {
                        body_events.push(BoxEvent::OpenExternalUrl(url));
                        *body_should_pop = true;
                    }
                }
            }
        }
    }
}
