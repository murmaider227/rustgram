//! Offline mock implementation of [`BotsBackend`] showcasing every bot button kind,
//! reply keyboard option, inline result kind (list + mosaic gallery), commands menu,
//! attach menu bots, URL auth, and HTML5 game score sharing.
//!
//! Port origin: offline test fixture for `api/api_bot.cpp`, `chat_helpers/bot_keyboard.cpp`,
//! `inline_bots/inline_results_widget.cpp`, and `boxes/url_auth_box.cpp`.

use tg_bots::callback::{
    AcceptUrlAuthParams, CallbackAnswerInput, CallbackQueryRequest, UrlAuthRequestData,
    UrlAuthResultInput, UrlAuthSource,
};
use tg_bots::commands::{
    BotCommand, BotCommandScope, BotInfo, BotMenuButton, BotStatus, ScopedBotCommands,
};
use tg_bots::inline::{
    GetInlineBotResultsRequest, InlineResult, InlineResultKind, InlineResultsPage, LocationPoint,
    MediaDimensions, SwitchPmButton,
};
use tg_bots::markup::{
    ButtonColor, ButtonKind, ButtonVisual, MarkupButton, PeerTypes, PollRequestRestriction,
    ReplyMarkupData, ReplyMarkupInput, RequestPeerCandidate, RequestPeerQuery,
    RequestPeerRestriction, RequestPeerType,
};
use tg_bots::mini_app::{
    AttachMenuBot, AttachMenuStore, AttachMenuToggleState, GameData, GameHighScore,
};
use tg_bots::stand_in::{
    ChatAdminRights, DocumentId, FullMsgId, GameId, MsgId, PeerId, PeerKind, PhotoId, UserId,
};

use crate::bots::backend::{
    BotChatMessage, BotChatSnapshot, BotMessageAttachment, BotPeerCandidate, BotsBackend,
    SubmittedPoll,
};

/// Demo bot user id (`@catalog_demo_bot`).
pub const DEMO_BOT_USER_ID: UserId = UserId(777_001);
/// Demo bot chat peer id.
pub const DEMO_BOT_PEER_ID: PeerId = PeerId(777_001);
/// Demo bot username.
pub const DEMO_BOT_USERNAME: &str = "catalog_demo_bot";
/// Demo message id carrying the comprehensive inline keyboard.
pub const DEMO_INLINE_KB_MSG_ID: MsgId = MsgId(101);
/// Demo message id carrying the HTML5 game card.
pub const DEMO_GAME_MSG_ID: MsgId = MsgId(102);
/// Demo message id owning the reply keyboard.
pub const DEMO_REPLY_KB_MSG_ID: MsgId = MsgId(103);

/// Offline [`BotsBackend`] populated with a fictional demo bot (`@catalog_demo_bot`).
pub struct MockBotsBackend {
    next_msg_id: i32,
    chat: BotChatSnapshot,
    attach_menu: AttachMenuStore,
    candidates: Vec<BotPeerCandidate>,
}

impl Default for MockBotsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBotsBackend {
    /// Creates a pre-populated mock backend containing every bot widget fixture.
    #[must_use]
    pub fn new() -> Self {
        let commands = vec![
            BotCommand::new("start", "Start the bot and show welcome card"),
            BotCommand::new("catalog", "Browse featured items and inline controls"),
            BotCommand::new(
                "keyboard",
                "Show custom reply keyboard with request buttons",
            ),
            BotCommand::new("game", "Launch Star Hopper arcade mini-game"),
            BotCommand::new("order", "Check order status and checkout actions"),
            BotCommand::new("settings", "Configure notifications and language"),
            BotCommand::new("privacy", "Read how this bot handles your data"),
        ];

        let mut scoped_commands = ScopedBotCommands::new();
        scoped_commands.set(BotCommandScope::Default, "", commands.clone());
        scoped_commands.set(BotCommandScope::Users, "en", commands.clone());

        let bot_info = BotInfo {
            description: "Explore interactive inline keyboards, custom reply keyboards, rich inline search (gallery & list), Mini Apps, OAuth login confirmation, and HTML5 games — all offline in Rustgram.".to_owned(),
            inline_placeholder: "Search photos, docs, venues, or music...".to_owned(),
            commands: commands.clone(),
            description_photo_id: Some(PhotoId(90_001)),
            description_document_id: None,
            menu_button: BotMenuButton::Commands,
            privacy_policy_url: "https://example.org/catalog-bot/privacy".to_owned(),
            active_users: 48_250,
            inited: true,
            supports_attach_menu: true,
            has_main_app: true,
            ..BotInfo::default()
        };

        let inline_markup = build_demo_inline_keyboard();
        let reply_markup = build_demo_reply_keyboard(true, false, true, false);
        let game = GameData {
            id: GameId(50_001),
            access_hash: 0x5A17_CAFE,
            short_name: "star_hopper".to_owned(),
            title: "Star Hopper".to_owned(),
            description:
                "Guide an orbital glider through asteroid rings and beat your group's high score."
                    .to_owned(),
            photo_id: Some(PhotoId(50_100)),
            document_id: None,
        };
        let high_scores = vec![
            GameHighScore {
                pos: 1,
                user_id: UserId(10_002),
                score: 4_820,
            },
            GameHighScore {
                pos: 2,
                user_id: UserId(10_001),
                score: 3_950,
            },
            GameHighScore {
                pos: 3,
                user_id: UserId(10_003),
                score: 2_740,
            },
        ];

        let game_markup = ReplyMarkupData::from_input(Some(ReplyMarkupInput::Inline {
            rows: vec![vec![
                MarkupButton::with_visual(
                    "Play Star Hopper",
                    ButtonVisual {
                        icon_id: DocumentId(0),
                        color: ButtonColor::Primary,
                    },
                    ButtonKind::Game,
                ),
                MarkupButton::new(
                    "Share Score",
                    ButtonKind::Callback {
                        data: b"share_game_score".to_vec(),
                    },
                ),
            ]],
            force_reply: false,
        }));

        let messages = vec![
            BotChatMessage {
                id: FullMsgId::new(DEMO_BOT_PEER_ID, MsgId(100)),
                author: "You".to_owned(),
                outgoing: true,
                time_label: "10:14".to_owned(),
                text: "/catalog".to_owned(),
                attachment: BotMessageAttachment::None,
                inline_markup: None,
                reply_markup: None,
                via_bot: None,
            },
            BotChatMessage {
                id: FullMsgId::new(DEMO_BOT_PEER_ID, DEMO_INLINE_KB_MSG_ID),
                author: "Catalog Assistant".to_owned(),
                outgoing: false,
                time_label: "10:14".to_owned(),
                text: "Welcome to Catalog Assistant! Try every inline button type below — callbacks (toast, alert, loading spinner, 2FA), external links, Login URL OAuth, Mini App WebView, inline switch, copy code, profile, and checkout.".to_owned(),
                attachment: BotMessageAttachment::None,
                inline_markup: Some(inline_markup),
                reply_markup: None,
                via_bot: None,
            },
            BotChatMessage {
                id: FullMsgId::new(DEMO_BOT_PEER_ID, DEMO_GAME_MSG_ID),
                author: "Catalog Assistant".to_owned(),
                outgoing: false,
                time_label: "10:15".to_owned(),
                text: "Weekly arcade challenge is live! Top 3 scores update automatically.".to_owned(),
                attachment: BotMessageAttachment::Game {
                    game,
                    high_scores,
                },
                inline_markup: Some(game_markup),
                reply_markup: None,
                via_bot: None,
            },
            BotChatMessage {
                id: FullMsgId::new(DEMO_BOT_PEER_ID, DEMO_REPLY_KB_MSG_ID),
                author: "Catalog Assistant".to_owned(),
                outgoing: false,
                time_label: "10:16".to_owned(),
                text: "Use the reply keyboard below to send quick actions or test phone, location, poll, and peer picker requests.".to_owned(),
                attachment: BotMessageAttachment::None,
                inline_markup: None,
                reply_markup: Some(reply_markup.clone()),
                via_bot: None,
            },
        ];

        let mut attach_menu = AttachMenuStore::new();
        attach_menu.update_bots(
            42,
            vec![
                AttachMenuBot {
                    bot_id: DEMO_BOT_USER_ID,
                    bot_peer_id: DEMO_BOT_PEER_ID,
                    icon_document_id: Some(DocumentId(88_001)),
                    short_name: "Catalog Shop".to_owned(),
                    peer_types: PeerTypes::SAME_BOT
                        | PeerTypes::BOT
                        | PeerTypes::USER
                        | PeerTypes::GROUP
                        | PeerTypes::BROADCAST,
                    inactive: false,
                    in_side_menu: true,
                    in_attach_menu: true,
                    disclaimer_required: false,
                    request_write_access: false,
                },
                AttachMenuBot {
                    bot_id: UserId(777_002),
                    bot_peer_id: PeerId(777_002),
                    icon_document_id: Some(DocumentId(88_002)),
                    short_name: "Quick Wallet".to_owned(),
                    peer_types: PeerTypes::SAME_BOT
                        | PeerTypes::BOT
                        | PeerTypes::USER
                        | PeerTypes::GROUP,
                    inactive: false,
                    in_side_menu: true,
                    in_attach_menu: true,
                    disclaimer_required: true,
                    request_write_access: true,
                },
                AttachMenuBot {
                    bot_id: UserId(777_003),
                    bot_peer_id: PeerId(777_003),
                    icon_document_id: Some(DocumentId(88_003)),
                    short_name: "Poll Studio".to_owned(),
                    peer_types: PeerTypes::SAME_BOT
                        | PeerTypes::BOT
                        | PeerTypes::GROUP
                        | PeerTypes::BROADCAST,
                    inactive: false,
                    in_side_menu: false,
                    in_attach_menu: true,
                    disclaimer_required: false,
                    request_write_access: false,
                },
                AttachMenuBot {
                    bot_id: UserId(777_004),
                    bot_peer_id: PeerId(777_004),
                    icon_document_id: Some(DocumentId(88_004)),
                    short_name: "Route Planner".to_owned(),
                    peer_types: PeerTypes::SAME_BOT
                        | PeerTypes::BOT
                        | PeerTypes::USER
                        | PeerTypes::GROUP,
                    inactive: true,
                    in_side_menu: false,
                    in_attach_menu: true,
                    disclaimer_required: true,
                    request_write_access: true,
                },
            ],
        );

        let candidates = vec![
            BotPeerCandidate {
                peer_id: PeerId(10_002),
                title: "Mira Lin".to_owned(),
                subtitle: "@mira_lin • Premium".to_owned(),
                peer_kind: PeerKind::User,
                is_self: false,
                can_send_games: true,
                is_forum: false,
                criteria: RequestPeerCandidate {
                    peer_type: RequestPeerType::User,
                    is_bot: false,
                    is_premium: true,
                    ..RequestPeerCandidate::default()
                },
            },
            BotPeerCandidate {
                peer_id: PeerId(10_003),
                title: "Theo Vance".to_owned(),
                subtitle: "@theo_v • Online".to_owned(),
                peer_kind: PeerKind::User,
                is_self: false,
                can_send_games: true,
                is_forum: false,
                criteria: RequestPeerCandidate {
                    peer_type: RequestPeerType::User,
                    is_bot: false,
                    is_premium: false,
                    ..RequestPeerCandidate::default()
                },
            },
            BotPeerCandidate {
                peer_id: PeerId(10_004),
                title: "Release Helper Bot".to_owned(),
                subtitle: "@release_helper_bot • Bot".to_owned(),
                peer_kind: PeerKind::Bot,
                is_self: false,
                can_send_games: false,
                is_forum: false,
                criteria: RequestPeerCandidate {
                    peer_type: RequestPeerType::User,
                    is_bot: true,
                    is_premium: false,
                    ..RequestPeerCandidate::default()
                },
            },
            BotPeerCandidate {
                peer_id: PeerId(20_001),
                title: "Rustgram Core Port".to_owned(),
                subtitle: "@rustgram_core • Forum • 128 members".to_owned(),
                peer_kind: PeerKind::Group,
                is_self: false,
                can_send_games: true,
                is_forum: true,
                criteria: RequestPeerCandidate {
                    peer_type: RequestPeerType::Group,
                    is_forum: true,
                    has_username: true,
                    am_creator: true,
                    is_bot_participant: true,
                    my_rights: ChatAdminRights::CHANGE_INFO
                        | ChatAdminRights::INVITE_BY_LINK_OR_ADD
                        | ChatAdminRights::PIN_MESSAGES,
                    bot_rights: ChatAdminRights::DELETE_MESSAGES | ChatAdminRights::PIN_MESSAGES,
                    ..RequestPeerCandidate::default()
                },
            },
            BotPeerCandidate {
                peer_id: PeerId(20_002),
                title: "Design Critique".to_owned(),
                subtitle: "Private group • 14 members".to_owned(),
                peer_kind: PeerKind::Group,
                is_self: false,
                can_send_games: true,
                is_forum: false,
                criteria: RequestPeerCandidate {
                    peer_type: RequestPeerType::Group,
                    is_forum: false,
                    has_username: false,
                    am_creator: false,
                    is_bot_participant: false,
                    my_rights: ChatAdminRights::INVITE_BY_LINK_OR_ADD,
                    bot_rights: ChatAdminRights::empty(),
                    ..RequestPeerCandidate::default()
                },
            },
            BotPeerCandidate {
                peer_id: PeerId(30_001),
                title: "Rustgram Dev Log".to_owned(),
                subtitle: "@rustgram_devlog • Channel • 2.4K subscribers".to_owned(),
                peer_kind: PeerKind::Broadcast,
                is_self: false,
                can_send_games: false,
                is_forum: false,
                criteria: RequestPeerCandidate {
                    peer_type: RequestPeerType::Broadcast,
                    has_username: true,
                    am_creator: true,
                    my_rights: ChatAdminRights::POST_MESSAGES | ChatAdminRights::EDIT_MESSAGES,
                    bot_rights: ChatAdminRights::POST_MESSAGES,
                    ..RequestPeerCandidate::default()
                },
            },
        ];

        Self {
            next_msg_id: 200,
            chat: BotChatSnapshot {
                peer_id: DEMO_BOT_PEER_ID,
                peer_kind: PeerKind::Bot,
                peer_bot_status: BotStatus::HasBots,
                bot_id: DEMO_BOT_USER_ID,
                bot_name: "Catalog Assistant".to_owned(),
                bot_username: DEMO_BOT_USERNAME.to_owned(),
                bot_verified: false,
                bot_trusted_for_game: false,
                bot_info,
                scoped_commands,
                current_user_name: "Alex Rivera".to_owned(),
                current_user_phone: "+1 555 010 0199".to_owned(),
                messages,
                active_reply_keyboard: Some((
                    FullMsgId::new(DEMO_BOT_PEER_ID, DEMO_REPLY_KB_MSG_ID),
                    reply_markup,
                )),
            },
            attach_menu,
            candidates,
        }
    }

    /// Replaces the active reply keyboard with custom flags (useful in tests and demo controls).
    pub fn configure_reply_keyboard(
        &mut self,
        resize: bool,
        single_use: bool,
        persistent: bool,
        selective: bool,
    ) {
        let kb = build_demo_reply_keyboard(resize, single_use, persistent, selective);
        self.chat.active_reply_keyboard =
            Some((FullMsgId::new(DEMO_BOT_PEER_ID, DEMO_REPLY_KB_MSG_ID), kb));
    }

    fn alloc_msg_id(&mut self, peer_id: PeerId) -> FullMsgId {
        self.next_msg_id += 1;
        FullMsgId::new(peer_id, MsgId(self.next_msg_id))
    }
}

/// Builds the demo inline keyboard covering every inline button kind & semantic color.
#[must_use]
pub fn build_demo_inline_keyboard() -> ReplyMarkupData {
    ReplyMarkupData::from_input(Some(ReplyMarkupInput::Inline {
        rows: vec![
            vec![
                MarkupButton::with_visual(
                    "Refresh Status",
                    ButtonVisual {
                        icon_id: DocumentId(0),
                        color: ButtonColor::Primary,
                    },
                    ButtonKind::Callback {
                        data: b"cb_toast".to_vec(),
                    },
                ),
                MarkupButton::with_visual(
                    "Show Alert",
                    ButtonVisual {
                        icon_id: DocumentId(0),
                        color: ButtonColor::Success,
                    },
                    ButtonKind::Callback {
                        data: b"cb_alert".to_vec(),
                    },
                ),
                MarkupButton::with_visual(
                    "2FA Action",
                    ButtonVisual {
                        icon_id: DocumentId(0),
                        color: ButtonColor::Danger,
                    },
                    ButtonKind::CallbackWithPassword {
                        data: b"cb_2fa".to_vec(),
                    },
                ),
            ],
            vec![
                MarkupButton::new(
                    "Open Docs",
                    ButtonKind::Url {
                        url: "https://example.org/rustgram/bot-guide".to_owned(),
                    },
                ),
                MarkupButton::new(
                    "Login via Telegram",
                    ButtonKind::Auth {
                        url: "https://portal.example.org/oauth/callback".to_owned(),
                        forward_text: "Open Portal".to_owned(),
                        button_id: 901,
                    },
                ),
            ],
            vec![
                MarkupButton::new(
                    "Open Store App",
                    ButtonKind::WebView {
                        url: "https://shop.example.org/miniapp".to_owned(),
                    },
                ),
                MarkupButton::new(
                    "Copy Promo Code",
                    ButtonKind::CopyText {
                        text: "RUSTGRAM-2026-BOT".to_owned(),
                    },
                ),
            ],
            vec![
                MarkupButton::new(
                    "Search Here",
                    ButtonKind::SwitchInlineSame {
                        query: "catalog".to_owned(),
                    },
                ),
                MarkupButton::new(
                    "Share Inline...",
                    ButtonKind::SwitchInline {
                        query: "photos".to_owned(),
                        peer_types: PeerTypes::USER | PeerTypes::GROUP,
                    },
                ),
            ],
            vec![
                MarkupButton::new(
                    "View Author",
                    ButtonKind::UserProfile {
                        user_id: UserId(10_002),
                    },
                ),
                MarkupButton::new("Buy Starter Pack", ButtonKind::Buy),
            ],
        ],
        force_reply: false,
    }))
}

/// Builds the demo reply keyboard with quick replies and request-phone/location/poll/peer buttons.
#[must_use]
pub fn build_demo_reply_keyboard(
    resize: bool,
    single_use: bool,
    persistent: bool,
    selective: bool,
) -> ReplyMarkupData {
    ReplyMarkupData::from_input(Some(ReplyMarkupInput::Keyboard {
        rows: vec![
            vec![
                MarkupButton::with_visual(
                    "Browse Catalog",
                    ButtonVisual {
                        icon_id: DocumentId(0),
                        color: ButtonColor::Primary,
                    },
                    ButtonKind::Default,
                ),
                MarkupButton::with_visual(
                    "My Orders",
                    ButtonVisual {
                        icon_id: DocumentId(0),
                        color: ButtonColor::Normal,
                    },
                    ButtonKind::Default,
                ),
            ],
            vec![
                MarkupButton::new("Share Phone", ButtonKind::RequestPhone),
                MarkupButton::new("Share Location", ButtonKind::RequestLocation),
                MarkupButton::new(
                    "Create Quiz Poll",
                    ButtonKind::RequestPoll {
                        restriction: PollRequestRestriction::QuizOnly,
                    },
                ),
            ],
            vec![
                MarkupButton::new(
                    "Select Group",
                    ButtonKind::RequestPeer {
                        button_id: 501,
                        query: RequestPeerQuery {
                            max_quantity: 2,
                            peer_type: RequestPeerType::Group,
                            group_is_forum: RequestPeerRestriction::Any,
                            has_username: RequestPeerRestriction::Any,
                            am_creator: false,
                            is_bot_participant: false,
                            my_rights: ChatAdminRights::INVITE_BY_LINK_OR_ADD,
                            bot_rights: ChatAdminRights::empty(),
                            user_is_bot: RequestPeerRestriction::Any,
                            user_is_premium: RequestPeerRestriction::Any,
                        },
                    },
                ),
                MarkupButton::new(
                    "Quick Web App",
                    ButtonKind::SimpleWebView {
                        url: "https://shop.example.org/quick".to_owned(),
                    },
                ),
            ],
        ],
        resize,
        single_use,
        selective,
        persistent,
        force_reply: false,
        placeholder: "Choose a bot command or type a message...".to_owned(),
    }))
}

impl BotsBackend for MockBotsBackend {
    fn load_bot_chat(&self) -> BotChatSnapshot {
        self.chat.clone()
    }

    fn load_attach_menu(&self) -> AttachMenuStore {
        self.attach_menu.clone()
    }

    fn candidate_peers(&self) -> Vec<BotPeerCandidate> {
        self.candidates.clone()
    }

    fn send_callback_query(
        &mut self,
        request: &CallbackQueryRequest,
    ) -> Result<CallbackAnswerInput, String> {
        if request.is_game {
            return Ok(CallbackAnswerInput {
                alert: false,
                has_url: true,
                native_ui: false,
                message: None,
                url: Some("https://games.example.org/star-hopper#score=3950".to_owned()),
                cache_time: 0,
            });
        }

        let payload = request.data.as_deref().unwrap_or(&[]);
        match payload {
            b"cb_toast" => Ok(CallbackAnswerInput {
                alert: false,
                has_url: false,
                native_ui: false,
                message: Some("Catalog synced: 12 items available (cached 15s)".to_owned()),
                url: None,
                cache_time: 15,
            }),
            b"cb_alert" => Ok(CallbackAnswerInput {
                alert: true,
                has_url: false,
                native_ui: false,
                message: Some(
                    "Order #4092 is confirmed and ready for pickup.\n\nThis is a modal callback alert (alert = true)."
                        .to_owned(),
                ),
                url: None,
                cache_time: 0,
            }),
            b"cb_2fa" => Ok(CallbackAnswerInput {
                alert: false,
                has_url: false,
                native_ui: false,
                message: Some("2FA-protected payout action approved.".to_owned()),
                url: None,
                cache_time: 0,
            }),
            b"share_game_score" => Ok(CallbackAnswerInput {
                alert: false,
                has_url: false,
                native_ui: false,
                message: Some("Opening Share Game Score chooser...".to_owned()),
                url: None,
                cache_time: 0,
            }),
            _ => Ok(CallbackAnswerInput {
                alert: false,
                has_url: false,
                native_ui: false,
                message: Some("Callback acknowledged by @catalog_demo_bot".to_owned()),
                url: None,
                cache_time: 5,
            }),
        }
    }

    fn request_url_auth(&mut self, source: &UrlAuthSource) -> Result<UrlAuthResultInput, String> {
        let domain = source
            .url()
            .strip_prefix("https://")
            .and_then(|s| s.split('/').next())
            .unwrap_or("portal.example.org")
            .to_owned();
        Ok(UrlAuthResultInput::Request(UrlAuthRequestData {
            domain,
            bot_id: self.chat.bot_id,
            bot_name: self.chat.bot_name.clone(),
            request_write_access: true,
            request_phone_number: false,
            match_codes_first: false,
            is_app: false,
            verified_app_name: None,
            user_id_hint: None,
            match_codes: vec!["A7K".to_owned(), "M4Q".to_owned(), "Z9R".to_owned()],
            browser: Some("Rustgram Desktop".to_owned()),
            platform: Some("Windows 11 x64".to_owned()),
            ip: Some("198.51.100.42".to_owned()),
            region: Some("Portland, US".to_owned()),
        }))
    }

    fn accept_url_auth(
        &mut self,
        params: &AcceptUrlAuthParams,
    ) -> Result<UrlAuthResultInput, String> {
        let write_flag = if params.write_allowed { "1" } else { "0" };
        let url = format!(
            "{}?tgAuthResult=demo_token_ok&write_allowed={write_flag}",
            params.source.url()
        );
        Ok(UrlAuthResultInput::Accepted { url: Some(url) })
    }

    fn decline_url_auth(&mut self, _url: &str) -> Result<(), String> {
        Ok(())
    }

    fn query_inline_results(
        &mut self,
        request: &GetInlineBotResultsRequest,
    ) -> Result<InlineResultsPage, String> {
        let q = request.query.trim().to_ascii_lowercase();
        let gallery = q.contains("photo")
            || q.contains("gif")
            || q.contains("sticker")
            || q.contains("gallery");

        if gallery {
            return Ok(build_gallery_inline_page(&request.query, &request.offset));
        }
        Ok(build_list_inline_page(&request.query, &request.offset))
    }

    fn send_inline_result(
        &mut self,
        _bot_id: UserId,
        peer_id: PeerId,
        result: &InlineResult,
    ) -> Result<BotChatMessage, String> {
        let id = self.alloc_msg_id(peer_id);
        let kind_label = format!("{:?}", result.kind);
        let title = if result.title.is_empty() {
            format!("{kind_label} #{}", result.id)
        } else {
            result.title.clone()
        };
        let subtitle = if result.description.is_empty() {
            result.url.clone()
        } else {
            result.description.clone()
        };
        let msg = BotChatMessage {
            id,
            author: "You".to_owned(),
            outgoing: true,
            time_label: "10:18".to_owned(),
            text: format!("Sent inline {kind_label}: {title}"),
            attachment: BotMessageAttachment::InlineSent {
                kind_label,
                title,
                subtitle,
            },
            inline_markup: result.reply_markup.clone(),
            reply_markup: None,
            via_bot: Some(format!("@{}", self.chat.bot_username)),
        };
        self.chat.messages.push(msg.clone());
        Ok(msg)
    }

    fn send_bot_command(
        &mut self,
        peer_id: PeerId,
        text: &str,
        _reply_to: Option<FullMsgId>,
    ) -> Result<BotChatMessage, String> {
        let id = self.alloc_msg_id(peer_id);
        let msg = BotChatMessage {
            id,
            author: "You".to_owned(),
            outgoing: true,
            time_label: "10:18".to_owned(),
            text: text.to_owned(),
            attachment: BotMessageAttachment::None,
            inline_markup: None,
            reply_markup: None,
            via_bot: None,
        };
        self.chat.messages.push(msg.clone());
        Ok(msg)
    }

    fn share_phone(
        &mut self,
        peer_id: PeerId,
        _reply_to: Option<MsgId>,
    ) -> Result<BotChatMessage, String> {
        let id = self.alloc_msg_id(peer_id);
        let msg = BotChatMessage {
            id,
            author: "You".to_owned(),
            outgoing: true,
            time_label: "10:19".to_owned(),
            text: format!(
                "Shared contact: {} ({})",
                self.chat.current_user_name, self.chat.current_user_phone
            ),
            attachment: BotMessageAttachment::None,
            inline_markup: None,
            reply_markup: None,
            via_bot: None,
        };
        self.chat.messages.push(msg.clone());
        Ok(msg)
    }

    fn send_poll(
        &mut self,
        peer_id: PeerId,
        poll: &SubmittedPoll,
    ) -> Result<BotChatMessage, String> {
        if poll.question.trim().is_empty() || poll.options.len() < 2 {
            return Err("Poll requires a question and at least two options.".to_owned());
        }
        let id = self.alloc_msg_id(peer_id);
        let kind = if poll.is_quiz { "Quiz" } else { "Poll" };
        let msg = BotChatMessage {
            id,
            author: "You".to_owned(),
            outgoing: true,
            time_label: "10:19".to_owned(),
            text: format!(
                "[{kind}] {} ({} options)",
                poll.question.trim(),
                poll.options.len()
            ),
            attachment: BotMessageAttachment::None,
            inline_markup: None,
            reply_markup: None,
            via_bot: None,
        };
        self.chat.messages.push(msg.clone());
        Ok(msg)
    }

    fn send_requested_peers(
        &mut self,
        item_id: FullMsgId,
        button_id: i64,
        selected_peers: &[PeerId],
    ) -> Result<BotChatMessage, String> {
        if selected_peers.is_empty() {
            return Err("Select at least one peer.".to_owned());
        }
        let names: Vec<String> = selected_peers
            .iter()
            .map(|pid| {
                self.candidates
                    .iter()
                    .find(|c| c.peer_id == *pid)
                    .map_or_else(|| format!("Peer {}", pid.0), |c| c.title.clone())
            })
            .collect();
        let id = self.alloc_msg_id(item_id.peer);
        let msg = BotChatMessage {
            id,
            author: "You".to_owned(),
            outgoing: true,
            time_label: "10:19".to_owned(),
            text: format!(
                "Shared peer(s) for button #{button_id}: {}",
                names.join(", ")
            ),
            attachment: BotMessageAttachment::None,
            inline_markup: None,
            reply_markup: None,
            via_bot: None,
        };
        self.chat.messages.push(msg.clone());
        Ok(msg)
    }

    fn toggle_attach_menu_bot(
        &mut self,
        bot_id: UserId,
        state: AttachMenuToggleState,
    ) -> Result<AttachMenuStore, String> {
        let Some(existing) = self
            .attach_menu
            .bots
            .iter_mut()
            .find(|b| b.bot_id == bot_id)
        else {
            return Err("Bot not found in attach menu.".to_owned());
        };
        match state {
            AttachMenuToggleState::Removed => existing.inactive = true,
            AttachMenuToggleState::Added | AttachMenuToggleState::AllowedToWrite => {
                existing.inactive = false;
            }
        }
        Ok(self.attach_menu.clone())
    }

    fn accept_attach_disclaimer(&mut self, bot_id: UserId) {
        self.attach_menu.mark_disclaimer_accepted(bot_id);
    }

    fn share_game_score(
        &mut self,
        _message_id: FullMsgId,
        target_peer: PeerId,
    ) -> Result<String, String> {
        let title = self
            .candidates
            .iter()
            .find(|c| c.peer_id == target_peer)
            .map_or_else(|| format!("Peer {}", target_peer.0), |c| c.title.clone());
        Ok(format!("Shared Star Hopper score (3,950 pts) to {title}"))
    }

    fn set_menu_button(&mut self, button: BotMenuButton) {
        self.chat.bot_info.apply_menu_button(button);
    }

    fn resolve_user_name(&self, user_id: UserId) -> String {
        match user_id.0 {
            10_001 => self.chat.current_user_name.clone(),
            10_002 => "Mira Lin".to_owned(),
            10_003 => "Theo Vance".to_owned(),
            _ => format!("Player #{}", user_id.0),
        }
    }

    fn commands_for_chat(&self) -> Vec<BotCommand> {
        self.chat.bot_info.commands.clone()
    }
}

#[doc(hidden)]
pub fn build_gallery_inline_page(query: &str, offset: &str) -> InlineResultsPage {
    let switch_pm = Some(SwitchPmButton::StartPm {
        text: "Configure Gallery Filters in Bot PM".to_owned(),
        start_param: "gallery_prefs".to_owned(),
    });
    if offset == "page_2" {
        return InlineResultsPage {
            query: query.to_owned(),
            next_offset: None,
            switch_pm,
            gallery: true,
            cache_time: 60,
            results: vec![
                InlineResult {
                    query_id: 9002,
                    id: "g_photo_5".to_owned(),
                    kind: InlineResultKind::Photo,
                    title: "Alpine Ridge".to_owned(),
                    description: "Wide landscape".to_owned(),
                    url: String::new(),
                    content_url: String::new(),
                    photo_id: Some(PhotoId(7005)),
                    document_id: None,
                    dimensions: MediaDimensions {
                        width: 1280,
                        height: 720,
                    },
                    has_thumbnail: true,
                    location: None,
                    duration_secs: None,
                    reply_markup: None,
                },
                InlineResult {
                    query_id: 9002,
                    id: "g_gif_2".to_owned(),
                    kind: InlineResultKind::Gif,
                    title: "Orbit Pulse".to_owned(),
                    description: "Looping GIF".to_owned(),
                    url: String::new(),
                    content_url: String::new(),
                    photo_id: None,
                    document_id: Some(DocumentId(7102)),
                    dimensions: MediaDimensions {
                        width: 480,
                        height: 480,
                    },
                    has_thumbnail: true,
                    location: None,
                    duration_secs: Some(3),
                    reply_markup: None,
                },
                InlineResult {
                    query_id: 9002,
                    id: "g_sticker_2".to_owned(),
                    kind: InlineResultKind::Sticker,
                    title: "Rocket Badge".to_owned(),
                    description: "Sticker".to_owned(),
                    url: String::new(),
                    content_url: String::new(),
                    photo_id: None,
                    document_id: Some(DocumentId(7202)),
                    dimensions: MediaDimensions {
                        width: 512,
                        height: 512,
                    },
                    has_thumbnail: true,
                    location: None,
                    duration_secs: None,
                    reply_markup: None,
                },
            ],
        };
    }

    InlineResultsPage {
        query: query.to_owned(),
        next_offset: Some("page_2".to_owned()),
        switch_pm,
        gallery: true,
        cache_time: 60,
        results: vec![
            InlineResult {
                query_id: 9001,
                id: "g_photo_1".to_owned(),
                kind: InlineResultKind::Photo,
                title: "Harbor Dawn".to_owned(),
                description: "16:9 photo".to_owned(),
                url: String::new(),
                content_url: String::new(),
                photo_id: Some(PhotoId(7001)),
                document_id: None,
                dimensions: MediaDimensions {
                    width: 1600,
                    height: 900,
                },
                has_thumbnail: true,
                location: None,
                duration_secs: None,
                reply_markup: None,
            },
            InlineResult {
                query_id: 9001,
                id: "g_photo_2".to_owned(),
                kind: InlineResultKind::Photo,
                title: "Cedar Trail".to_owned(),
                description: "Portrait photo".to_owned(),
                url: String::new(),
                content_url: String::new(),
                photo_id: Some(PhotoId(7002)),
                document_id: None,
                dimensions: MediaDimensions {
                    width: 720,
                    height: 1080,
                },
                has_thumbnail: true,
                location: None,
                duration_secs: None,
                reply_markup: None,
            },
            InlineResult {
                query_id: 9001,
                id: "g_gif_1".to_owned(),
                kind: InlineResultKind::Gif,
                title: "Neon Wave".to_owned(),
                description: "Animated GIF".to_owned(),
                url: String::new(),
                content_url: String::new(),
                photo_id: None,
                document_id: Some(DocumentId(7101)),
                dimensions: MediaDimensions {
                    width: 640,
                    height: 480,
                },
                has_thumbnail: true,
                location: None,
                duration_secs: Some(4),
                reply_markup: None,
            },
            InlineResult {
                query_id: 9001,
                id: "g_photo_3".to_owned(),
                kind: InlineResultKind::Photo,
                title: "Cloud Deck".to_owned(),
                description: "Square photo".to_owned(),
                url: String::new(),
                content_url: String::new(),
                photo_id: Some(PhotoId(7003)),
                document_id: None,
                dimensions: MediaDimensions {
                    width: 900,
                    height: 900,
                },
                has_thumbnail: true,
                location: None,
                duration_secs: None,
                reply_markup: None,
            },
            InlineResult {
                query_id: 9001,
                id: "g_photo_4".to_owned(),
                kind: InlineResultKind::Photo,
                title: "Dusk Lantern".to_owned(),
                description: "4:3 photo".to_owned(),
                url: String::new(),
                content_url: String::new(),
                photo_id: Some(PhotoId(7004)),
                document_id: None,
                dimensions: MediaDimensions {
                    width: 1200,
                    height: 900,
                },
                has_thumbnail: true,
                location: None,
                duration_secs: None,
                reply_markup: None,
            },
            InlineResult {
                query_id: 9001,
                id: "g_sticker_1".to_owned(),
                kind: InlineResultKind::Sticker,
                title: "Sparkle Bot".to_owned(),
                description: "Sticker".to_owned(),
                url: String::new(),
                content_url: String::new(),
                photo_id: None,
                document_id: Some(DocumentId(7201)),
                dimensions: MediaDimensions {
                    width: 512,
                    height: 512,
                },
                has_thumbnail: true,
                location: None,
                duration_secs: None,
                reply_markup: None,
            },
        ],
    }
}

#[doc(hidden)]
pub fn build_list_inline_page(query: &str, offset: &str) -> InlineResultsPage {
    let switch_pm = Some(SwitchPmButton::WebView {
        text: "Open Full Catalog Mini App".to_owned(),
        url: "https://shop.example.org/inline-search".to_owned(),
    });

    if offset == "page_2" {
        return InlineResultsPage {
            query: query.to_owned(),
            next_offset: None,
            switch_pm,
            gallery: false,
            cache_time: 30,
            results: vec![
                InlineResult {
                    query_id: 8002,
                    id: "res_contact".to_owned(),
                    kind: InlineResultKind::Contact,
                    title: "Support Desk".to_owned(),
                    description: "+1 555 019 2048".to_owned(),
                    url: String::new(),
                    content_url: String::new(),
                    photo_id: None,
                    document_id: None,
                    dimensions: MediaDimensions::default(),
                    has_thumbnail: false,
                    location: None,
                    duration_secs: None,
                    reply_markup: None,
                },
                InlineResult {
                    query_id: 8002,
                    id: "res_location".to_owned(),
                    kind: InlineResultKind::Location,
                    title: "Observatory Point".to_owned(),
                    description: "45.5231, -122.6765".to_owned(),
                    url: String::new(),
                    content_url: String::new(),
                    photo_id: None,
                    document_id: None,
                    dimensions: MediaDimensions::default(),
                    has_thumbnail: false,
                    location: Some(LocationPoint {
                        lat: 45.5231,
                        lon: -122.6765,
                        access_hash: 11,
                    }),
                    duration_secs: None,
                    reply_markup: None,
                },
                InlineResult {
                    query_id: 8002,
                    id: "res_venue".to_owned(),
                    kind: InlineResultKind::Venue,
                    title: "Harbor Coffee Roasters".to_owned(),
                    description: "420 Riverfront Ave, Suite 4".to_owned(),
                    url: String::new(),
                    content_url: String::new(),
                    photo_id: None,
                    document_id: None,
                    dimensions: MediaDimensions::default(),
                    has_thumbnail: false,
                    location: Some(LocationPoint {
                        lat: 45.5189,
                        lon: -122.6741,
                        access_hash: 12,
                    }),
                    duration_secs: None,
                    reply_markup: None,
                },
                InlineResult {
                    query_id: 8002,
                    id: "res_game".to_owned(),
                    kind: InlineResultKind::Game,
                    title: "Star Hopper".to_owned(),
                    description: "Arcade glider • Play with friends in this chat".to_owned(),
                    url: String::new(),
                    content_url: String::new(),
                    photo_id: Some(PhotoId(50_100)),
                    document_id: None,
                    dimensions: MediaDimensions {
                        width: 640,
                        height: 360,
                    },
                    has_thumbnail: true,
                    location: None,
                    duration_secs: None,
                    reply_markup: None,
                },
            ],
        };
    }

    let mut all = vec![
        InlineResult {
            query_id: 8001,
            id: "res_article_thumb".to_owned(),
            kind: InlineResultKind::Article,
            title: "Rustgram Bot API Guide".to_owned(),
            description: "Inline keyboards, reply markups, and mosaic galleries in Rust."
                .to_owned(),
            url: "https://docs.example.org/bots/overview".to_owned(),
            content_url: String::new(),
            photo_id: Some(PhotoId(8101)),
            document_id: None,
            dimensions: MediaDimensions {
                width: 320,
                height: 320,
            },
            has_thumbnail: true,
            location: None,
            duration_secs: None,
            reply_markup: None,
        },
        InlineResult {
            query_id: 8001,
            id: "res_article_letter".to_owned(),
            kind: InlineResultKind::Article,
            title: "Release Notes v0.9".to_owned(),
            description: "No thumbnail — uses domain monogram fallback letter.".to_owned(),
            url: "https://changelog.rustdesk.example.org/0.9".to_owned(),
            content_url: String::new(),
            photo_id: None,
            document_id: None,
            dimensions: MediaDimensions::default(),
            has_thumbnail: false,
            location: None,
            duration_secs: None,
            reply_markup: None,
        },
        InlineResult {
            query_id: 8001,
            id: "res_video".to_owned(),
            kind: InlineResultKind::Video,
            title: "Demo Walkthrough (1080p)".to_owned(),
            description: "MP4 • 18.4 MB".to_owned(),
            url: String::new(),
            content_url: String::new(),
            photo_id: None,
            document_id: Some(DocumentId(8201)),
            dimensions: MediaDimensions {
                width: 1280,
                height: 720,
            },
            has_thumbnail: true,
            location: None,
            duration_secs: Some(134),
            reply_markup: None,
        },
        InlineResult {
            query_id: 8001,
            id: "res_audio".to_owned(),
            kind: InlineResultKind::Audio,
            title: "Midnight Telemetry".to_owned(),
            description: "Synth Collective • FLAC".to_owned(),
            url: String::new(),
            content_url: String::new(),
            photo_id: None,
            document_id: Some(DocumentId(8202)),
            dimensions: MediaDimensions::default(),
            has_thumbnail: false,
            location: None,
            duration_secs: Some(222),
            reply_markup: None,
        },
        InlineResult {
            query_id: 8001,
            id: "res_voice".to_owned(),
            kind: InlineResultKind::Voice,
            title: "Voice Note Preview".to_owned(),
            description: "Ogg Opus • 48 kHz".to_owned(),
            url: String::new(),
            content_url: String::new(),
            photo_id: None,
            document_id: Some(DocumentId(8203)),
            dimensions: MediaDimensions::default(),
            has_thumbnail: false,
            location: None,
            duration_secs: Some(18),
            reply_markup: None,
        },
        InlineResult {
            query_id: 8001,
            id: "res_document".to_owned(),
            kind: InlineResultKind::Document,
            title: "bot_architecture_spec.pdf".to_owned(),
            description: "PDF Document • 1.2 MB".to_owned(),
            url: String::new(),
            content_url: String::new(),
            photo_id: None,
            document_id: Some(DocumentId(8204)),
            dimensions: MediaDimensions::default(),
            has_thumbnail: false,
            location: None,
            duration_secs: None,
            reply_markup: None,
        },
        InlineResult {
            query_id: 8001,
            id: "res_contact_1".to_owned(),
            kind: InlineResultKind::Contact,
            title: "Support Desk".to_owned(),
            description: "+1 555 019 2048".to_owned(),
            url: String::new(),
            content_url: String::new(),
            photo_id: None,
            document_id: None,
            dimensions: MediaDimensions::default(),
            has_thumbnail: false,
            location: None,
            duration_secs: None,
            reply_markup: None,
        },
        InlineResult {
            query_id: 8001,
            id: "res_venue_1".to_owned(),
            kind: InlineResultKind::Venue,
            title: "Harbor Coffee Roasters".to_owned(),
            description: "420 Riverfront Ave • 45.5189, -122.6741".to_owned(),
            url: String::new(),
            content_url: String::new(),
            photo_id: None,
            document_id: None,
            dimensions: MediaDimensions::default(),
            has_thumbnail: false,
            location: Some(LocationPoint {
                lat: 45.5189,
                lon: -122.6741,
                access_hash: 12,
            }),
            duration_secs: None,
            reply_markup: None,
        },
        InlineResult {
            query_id: 8001,
            id: "res_game_1".to_owned(),
            kind: InlineResultKind::Game,
            title: "Star Hopper".to_owned(),
            description: "HTML5 Arcade Game • High Score: 4,820".to_owned(),
            url: String::new(),
            content_url: String::new(),
            photo_id: Some(PhotoId(50_100)),
            document_id: None,
            dimensions: MediaDimensions {
                width: 640,
                height: 360,
            },
            has_thumbnail: true,
            location: None,
            duration_secs: None,
            reply_markup: None,
        },
    ];

    let trimmed = query.trim().to_ascii_lowercase();
    if !trimmed.is_empty() && trimmed != "catalog" && trimmed != "all" {
        let filtered: Vec<InlineResult> = all
            .iter()
            .filter(|r| {
                r.title.to_ascii_lowercase().contains(&trimmed)
                    || r.description.to_ascii_lowercase().contains(&trimmed)
                    || format!("{:?}", r.kind)
                        .to_ascii_lowercase()
                        .contains(&trimmed)
            })
            .cloned()
            .collect();
        if !filtered.is_empty() {
            all = filtered;
        }
    }

    InlineResultsPage {
        query: query.to_owned(),
        next_offset: Some("page_2".to_owned()),
        switch_pm,
        gallery: false,
        cache_time: 30,
        results: all,
    }
}
