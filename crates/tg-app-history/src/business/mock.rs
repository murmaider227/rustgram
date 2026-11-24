//! Deterministic offline fake implementation of [`BusinessBackend`].
//!
//! Supplies realistic synthetic business settings, opening hours, quick replies,
//! business chat links, chatbot candidates, Premium features, limits, and emoji statuses
//! without any network calls, real accounts, real people's names, or real billing.

use std::collections::BTreeMap;

use crate::business::backend::BusinessBackend;
use crate::business::model::{
    AWAY_START_TIME_MIN, ActiveEmojiStatus, AwaySchedule, AwayScheduleType, AwaySettings,
    BotLookupState, BotSearchResult, BusinessBotCandidate, BusinessChatLink, BusinessChatTypes,
    BusinessChats, BusinessDetails, BusinessLocation, BusinessOverviewSnapshot, BusinessRecipients,
    BusinessShortcutId, BusinessUserRef, CHAT_INTRO_DESCRIPTION_LIMIT, CHAT_INTRO_TITLE_LIMIT,
    CHAT_LINKS_LIMIT, ChatIntro, ChatbotsPermissions, ChatbotsSettings, DEFAULT_NO_ACTIVITY_DAYS,
    EmojiStatusCategory, EmojiStatusDuration, EmojiStatusItem, GeoPoint, GreetingSettings,
    InactiveChannelEntry, IntroStickerOption, MAX_SEARCH_BOTS, PremiumFeatureEntry,
    PremiumLimitsData, PremiumPlanOption, PremiumTierComparisonRow, PublicLinkChannelEntry,
    QUICK_REPLIES_LIMIT, QuickReplyAttachment, QuickReplyMessage, QuickReplyShortcut,
    SECONDS_PER_DAY, SHORTCUT_MESSAGES_LIMIT, Timezone, WorkingHours, WorkingInterval,
    WorkingIntervals, parse_bot_username_query, valid_shortcut_name,
};

/// Offline fake backend implementing [`BusinessBackend`] for interactive UI development,
/// tests, and deterministic screenshots.
pub struct MockBusinessBackend {
    snapshot: BusinessOverviewSnapshot,
    bot_directory: Vec<BusinessBotCandidate>,
    next_shortcut_id: BusinessShortcutId,
    next_message_id: i32,
    next_link_seq: u32,
}

impl Default for MockBusinessBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBusinessBackend {
    /// Creates a pre-populated offline [`MockBusinessBackend`] with synthetic demo data.
    #[must_use]
    pub fn new() -> Self {
        let timezones = default_timezones();
        let available_users = default_users();
        let bot_directory = default_bots();

        // Mon-Fri 09:00-18:00, Sat 10:00-16:00, Sun Closed (seconds from Monday 00:00).
        let mut raw_intervals = Vec::new();
        for day in 0..5 {
            raw_intervals.push(WorkingInterval::new(
                day * SECONDS_PER_DAY + 9 * 3600,
                day * SECONDS_PER_DAY + 18 * 3600,
            ));
        }
        // Saturday 10:00 - 16:00
        raw_intervals.push(WorkingInterval::new(
            5 * SECONDS_PER_DAY + 10 * 3600,
            5 * SECONDS_PER_DAY + 16 * 3600,
        ));
        let hours = WorkingHours {
            intervals: WorkingIntervals::new(raw_intervals).normalized(),
            timezone_id: "America/New_York".to_owned(),
        };

        let location = BusinessLocation {
            address: "420 Cedar Workshop Ave, Suite 3B, Portland, OR".to_owned(),
            point: Some(GeoPoint {
                lat: 45.5231,
                lon: -122.6765,
            }),
        };

        let intro = ChatIntro {
            title: "Northwind Design Studio".to_owned(),
            description: "Ask about availability, custom orders, or studio bookings anytime."
                .to_owned(),
            sticker_id: Some(9001),
        };

        let mut shortcuts = BTreeMap::new();
        shortcuts.insert(
            1,
            QuickReplyShortcut {
                id: 1,
                name: "hello".to_owned(),
                messages: vec![QuickReplyMessage {
                    id: 101,
                    text: "Welcome to Northwind Studio! How can we help with your project today?"
                        .to_owned(),
                    attachment: QuickReplyAttachment::None,
                    time_label: "09:00".to_owned(),
                }],
            },
        );
        shortcuts.insert(
            2,
            QuickReplyShortcut {
                id: 2,
                name: "away".to_owned(),
                messages: vec![QuickReplyMessage {
                    id: 102,
                    text: "Thanks for writing! Our team is currently away — we reply within 1 business day."
                        .to_owned(),
                    attachment: QuickReplyAttachment::None,
                    time_label: "18:05".to_owned(),
                }],
            },
        );
        shortcuts.insert(
            3,
            QuickReplyShortcut {
                id: 3,
                name: "pricing".to_owned(),
                messages: vec![
                    QuickReplyMessage {
                        id: 103,
                        text: "Here is our current 2026 studio rate card and turnaround guide:"
                            .to_owned(),
                        attachment: QuickReplyAttachment::Document,
                        time_label: "11:20".to_owned(),
                    },
                    QuickReplyMessage {
                        id: 104,
                        text: "Starter Package: $450 (3 days) • Full Brand Kit: $1,250 (7 days)"
                            .to_owned(),
                        attachment: QuickReplyAttachment::None,
                        time_label: "11:20".to_owned(),
                    },
                ],
            },
        );
        shortcuts.insert(
            4,
            QuickReplyShortcut {
                id: 4,
                name: "thanks".to_owned(),
                messages: vec![QuickReplyMessage {
                    id: 105,
                    text: "Thank you for confirming! We have queued your request and will share a preview soon."
                        .to_owned(),
                    attachment: QuickReplyAttachment::None,
                    time_label: "14:40".to_owned(),
                }],
            },
        );
        shortcuts.insert(
            5,
            QuickReplyShortcut {
                id: 5,
                name: "hours".to_owned(),
                messages: vec![QuickReplyMessage {
                    id: 106,
                    text: "Studio hours: Mon-Fri 09:00-18:00, Sat 10:00-16:00 (UTC-5). Closed Sundays."
                        .to_owned(),
                    attachment: QuickReplyAttachment::Photo,
                    time_label: "15:10".to_owned(),
                }],
            },
        );

        let greeting = GreetingSettings {
            recipients: BusinessRecipients {
                included: BusinessChats::default(),
                excluded: BusinessChats {
                    types: BusinessChatTypes::CONTACTS,
                    user_ids: vec![2003],
                },
                all_but_excluded: true,
            },
            no_activity_days: DEFAULT_NO_ACTIVITY_DAYS,
            shortcut_id: 1,
        };

        let away = AwaySettings {
            recipients: BusinessRecipients {
                included: BusinessChats::default(),
                excluded: BusinessChats::default(),
                all_but_excluded: true,
            },
            schedule: AwaySchedule {
                schedule_type: AwayScheduleType::OutsideWorkingHours,
                custom_interval: WorkingInterval::new(1_795_300_000, 1_795_386_400),
            },
            shortcut_id: 2,
            offline_only: true,
        };

        let chatbots = ChatbotsSettings {
            bot: Some(bot_directory[0].clone()),
            recipients: BusinessRecipients {
                included: BusinessChats {
                    types: BusinessChatTypes::NEW_CHATS.union(BusinessChatTypes::NON_CONTACTS),
                    user_ids: vec![],
                },
                excluded: BusinessChats {
                    types: BusinessChatTypes::EMPTY,
                    user_ids: vec![2001],
                },
                all_but_excluded: false,
            },
            permissions: ChatbotsPermissions::VIEW_MESSAGES
                .union(ChatbotsPermissions::REPLY_TO_MESSAGES)
                .union(ChatbotsPermissions::MARK_AS_READ),
        };

        let chat_links = vec![
            BusinessChatLink {
                link: "https://t.me/m/nWr8Kx2Q".to_owned(),
                title: "Website Quote Inquiry".to_owned(),
                message: "Hi! I saw your portfolio and would like a project estimate.".to_owned(),
                clicks: 148,
            },
            BusinessChatLink {
                link: "https://t.me/m/pL4vTz9M".to_owned(),
                title: "Workshop Booking".to_owned(),
                message: "Hello, I want to reserve a spot for Saturday's workshop.".to_owned(),
                clicks: 54,
            },
            BusinessChatLink {
                link: "https://t.me/m/cH7qBm3R".to_owned(),
                title: String::new(),
                message: "Order status question for reference #".to_owned(),
                clicks: 0,
            },
        ];

        let snapshot = BusinessOverviewSnapshot {
            account_name: "Northwind Studio".to_owned(),
            account_username: "northwind_studio".to_owned(),
            account_phone_display: "+1 555 010 0199".to_owned(),
            is_premium: true,
            sponsored_disabled: true,
            details: BusinessDetails {
                hours,
                location,
                intro,
            },
            timezones,
            greeting,
            away,
            chatbots,
            shortcuts,
            chat_links,
            available_users,
            intro_stickers: default_intro_stickers(),
            emoji_status: ActiveEmojiStatus {
                status_id: Some(7002),
                duration: EmojiStatusDuration::Forever,
            },
            emoji_statuses: default_emoji_statuses(),
            premium_limits: PremiumLimitsData::default(),
            premium_features: default_premium_features(),
            premium_plans: default_premium_plans(),
            tier_comparison: default_tier_comparison(),
            inactive_channels: default_inactive_channels(),
            public_link_channels: default_public_link_channels(),
        };

        Self {
            snapshot,
            bot_directory,
            next_shortcut_id: 10,
            next_message_id: 200,
            next_link_seq: 4,
        }
    }
}

impl BusinessBackend for MockBusinessBackend {
    fn load_overview(&self) -> Result<BusinessOverviewSnapshot, String> {
        Ok(self.snapshot.clone())
    }

    fn set_premium_enabled(&mut self, enabled: bool) -> Result<(), String> {
        self.snapshot.is_premium = enabled;
        Ok(())
    }

    fn set_sponsored_disabled(&mut self, disabled: bool) -> Result<(), String> {
        self.snapshot.sponsored_disabled = disabled;
        Ok(())
    }

    fn save_working_hours(&mut self, hours: WorkingHours) -> Result<(), String> {
        self.snapshot.details.hours = hours.normalized();
        Ok(())
    }

    fn save_location(&mut self, mut location: BusinessLocation) -> Result<(), String> {
        location.address = location.address.trim().to_owned();
        if location.address.is_empty() {
            location.point = None;
        }
        self.snapshot.details.location = location;
        Ok(())
    }

    fn save_chat_intro(&mut self, intro: ChatIntro) -> Result<(), String> {
        if intro.title.chars().count() > CHAT_INTRO_TITLE_LIMIT {
            return Err(format!(
                "Intro title exceeds {CHAT_INTRO_TITLE_LIMIT} characters."
            ));
        }
        if intro.description.chars().count() > CHAT_INTRO_DESCRIPTION_LIMIT {
            return Err(format!(
                "Intro description exceeds {CHAT_INTRO_DESCRIPTION_LIMIT} characters."
            ));
        }
        self.snapshot.details.intro = intro;
        Ok(())
    }

    fn save_greeting_settings(&mut self, mut settings: GreetingSettings) -> Result<(), String> {
        if settings.is_enabled() {
            settings.recipients = settings.recipients.make_valid();
            if settings.shortcut_id == 0
                && let Some(sc) = self.snapshot.shortcuts.values().find(|s| s.name == "hello")
            {
                settings.shortcut_id = sc.id;
            }
        }
        self.snapshot.greeting = settings;
        Ok(())
    }

    fn save_away_settings(&mut self, mut settings: AwaySettings) -> Result<(), String> {
        if settings.is_enabled() {
            settings.recipients = settings.recipients.make_valid();
            if settings.schedule.schedule_type == AwayScheduleType::Custom
                && AwaySchedule::is_bad_custom_interval(settings.schedule.custom_interval)
            {
                return Err(format!(
                    "Custom schedule must start after timestamp {AWAY_START_TIME_MIN} with start < end."
                ));
            }
            if settings.shortcut_id == 0
                && let Some(sc) = self.snapshot.shortcuts.values().find(|s| s.name == "away")
            {
                settings.shortcut_id = sc.id;
            }
        }
        self.snapshot.away = settings;
        Ok(())
    }

    fn search_business_bots(&self, query: &str) -> BotSearchResult {
        let parsed = parse_bot_username_query(query);
        if !parsed.valid {
            return BotSearchResult::default();
        }
        let needle = parsed.value.to_lowercase();
        let mut exact_unsupported = false;
        let mut bots = Vec::new();
        for bot in &self.bot_directory {
            let uname = bot.username.to_lowercase();
            let name = bot.name.to_lowercase();
            if uname == needle && !bot.supports_business {
                exact_unsupported = true;
                continue;
            }
            if bot.supports_business && (uname.contains(&needle) || name.contains(&needle)) {
                bots.push(bot.clone());
                if bots.len() >= MAX_SEARCH_BOTS {
                    break;
                }
            }
        }
        let state = if bots.is_empty() && exact_unsupported {
            BotLookupState::Unsupported
        } else {
            BotLookupState::Ready
        };
        BotSearchResult { bots, state }
    }

    fn save_chatbots(&mut self, mut settings: ChatbotsSettings) -> Result<(), String> {
        if settings.bot.is_some() {
            settings.recipients = settings.recipients.make_valid();
            settings
                .permissions
                .set(ChatbotsPermissions::VIEW_MESSAGES, true);
        }
        self.snapshot.chatbots = settings;
        Ok(())
    }

    fn emplace_shortcut(&mut self, name: &str) -> Result<QuickReplyShortcut, String> {
        let trimmed = name.trim();
        if !valid_shortcut_name(trimmed) {
            return Err("Shortcut name must be 1-32 letters, numbers, or underscores.".to_owned());
        }
        if let Some(existing) = self
            .snapshot
            .shortcuts
            .values()
            .find(|s| s.name.eq_ignore_ascii_case(trimmed))
        {
            return Ok(existing.clone());
        }
        let non_empty_count = self
            .snapshot
            .shortcuts
            .values()
            .filter(|s| !s.messages.is_empty())
            .count();
        if non_empty_count >= QUICK_REPLIES_LIMIT {
            return Err(format!(
                "Quick replies limit ({QUICK_REPLIES_LIMIT}) reached."
            ));
        }
        let id = self.next_shortcut_id;
        self.next_shortcut_id += 1;
        let sc = QuickReplyShortcut {
            id,
            name: trimmed.to_owned(),
            messages: Vec::new(),
        };
        self.snapshot.shortcuts.insert(id, sc.clone());
        Ok(sc)
    }

    fn rename_shortcut(
        &mut self,
        shortcut_id: BusinessShortcutId,
        new_name: &str,
    ) -> Result<QuickReplyShortcut, String> {
        let trimmed = new_name.trim();
        if !valid_shortcut_name(trimmed) {
            return Err("Shortcut name must be 1-32 letters, numbers, or underscores.".to_owned());
        }
        if self
            .snapshot
            .shortcuts
            .values()
            .any(|s| s.id != shortcut_id && s.name.eq_ignore_ascii_case(trimmed))
        {
            return Err(format!("Shortcut /{trimmed} already exists."));
        }
        let Some(sc) = self.snapshot.shortcuts.get_mut(&shortcut_id) else {
            return Err("Shortcut not found.".to_owned());
        };
        sc.name = trimmed.to_owned();
        Ok(sc.clone())
    }

    fn remove_shortcut(&mut self, shortcut_id: BusinessShortcutId) -> Result<(), String> {
        self.snapshot.shortcuts.remove(&shortcut_id);
        if self.snapshot.greeting.shortcut_id == shortcut_id {
            self.snapshot.greeting.no_activity_days = 0;
            self.snapshot.greeting.shortcut_id = 0;
        }
        if self.snapshot.away.shortcut_id == shortcut_id {
            self.snapshot.away.schedule.schedule_type = AwayScheduleType::Never;
            self.snapshot.away.shortcut_id = 0;
        }
        Ok(())
    }

    fn add_shortcut_message(
        &mut self,
        shortcut_id: BusinessShortcutId,
        text: &str,
        attachment: QuickReplyAttachment,
    ) -> Result<QuickReplyMessage, String> {
        let trimmed = text.trim();
        if trimmed.is_empty() && attachment == QuickReplyAttachment::None {
            return Err("Message text cannot be empty.".to_owned());
        }
        let Some(sc) = self.snapshot.shortcuts.get_mut(&shortcut_id) else {
            return Err("Shortcut not found.".to_owned());
        };
        if sc.messages.len() >= SHORTCUT_MESSAGES_LIMIT {
            return Err(format!(
                "Maximum {SHORTCUT_MESSAGES_LIMIT} messages per shortcut."
            ));
        }
        let id = self.next_message_id;
        self.next_message_id += 1;
        let msg = QuickReplyMessage {
            id,
            text: trimmed.to_owned(),
            attachment,
            time_label: "Now".to_owned(),
        };
        sc.messages.push(msg.clone());
        Ok(msg)
    }

    fn edit_shortcut_message(
        &mut self,
        shortcut_id: BusinessShortcutId,
        message_id: i32,
        new_text: &str,
    ) -> Result<QuickReplyMessage, String> {
        let trimmed = new_text.trim();
        let Some(sc) = self.snapshot.shortcuts.get_mut(&shortcut_id) else {
            return Err("Shortcut not found.".to_owned());
        };
        let Some(msg) = sc.messages.iter_mut().find(|m| m.id == message_id) else {
            return Err("Message not found.".to_owned());
        };
        if trimmed.is_empty() && msg.attachment == QuickReplyAttachment::None {
            return Err("Message text cannot be empty.".to_owned());
        }
        msg.text = trimmed.to_owned();
        Ok(msg.clone())
    }

    fn delete_shortcut_message(
        &mut self,
        shortcut_id: BusinessShortcutId,
        message_id: i32,
    ) -> Result<(), String> {
        let Some(sc) = self.snapshot.shortcuts.get_mut(&shortcut_id) else {
            return Err("Shortcut not found.".to_owned());
        };
        sc.messages.retain(|m| m.id != message_id);
        Ok(())
    }

    fn create_chat_link(&mut self, title: &str, message: &str) -> Result<BusinessChatLink, String> {
        if self.snapshot.chat_links.len() >= CHAT_LINKS_LIMIT {
            return Err(format!("Maximum {CHAT_LINKS_LIMIT} chat links reached."));
        }
        let seq = self.next_link_seq;
        self.next_link_seq += 1;
        let link = BusinessChatLink {
            link: format!("https://t.me/m/demoLink{seq:02}X"),
            title: title.trim().to_owned(),
            message: message.trim().to_owned(),
            clicks: 0,
        };
        self.snapshot.chat_links.insert(0, link.clone());
        Ok(link)
    }

    fn edit_chat_link(
        &mut self,
        link: &str,
        title: &str,
        message: &str,
    ) -> Result<BusinessChatLink, String> {
        let Some(existing) = self
            .snapshot
            .chat_links
            .iter_mut()
            .find(|item| item.link == link)
        else {
            return Err("Chat link not found.".to_owned());
        };
        existing.title = title.trim().to_owned();
        existing.message = message.trim().to_owned();
        Ok(existing.clone())
    }

    fn delete_chat_link(&mut self, link: &str) -> Result<(), String> {
        self.snapshot.chat_links.retain(|item| item.link != link);
        Ok(())
    }

    fn set_emoji_status(&mut self, status: ActiveEmojiStatus) -> Result<(), String> {
        self.snapshot.emoji_status = status;
        Ok(())
    }

    fn leave_inactive_channels(&mut self, channel_ids: &[i64]) -> Result<usize, String> {
        let before = self.snapshot.inactive_channels.len();
        self.snapshot
            .inactive_channels
            .retain(|ch| !channel_ids.contains(&ch.id));
        Ok(before.saturating_sub(self.snapshot.inactive_channels.len()))
    }

    fn revoke_public_link(&mut self, channel_id: i64) -> Result<(), String> {
        self.snapshot
            .public_link_channels
            .retain(|ch| ch.id != channel_id);
        Ok(())
    }
}

fn default_timezones() -> Vec<Timezone> {
    vec![
        Timezone {
            id: "America/Los_Angeles".to_owned(),
            name: "Los Angeles".to_owned(),
            utc_offset: -8 * 3600,
        },
        Timezone {
            id: "America/Denver".to_owned(),
            name: "Denver".to_owned(),
            utc_offset: -7 * 3600,
        },
        Timezone {
            id: "America/Chicago".to_owned(),
            name: "Chicago".to_owned(),
            utc_offset: -6 * 3600,
        },
        Timezone {
            id: "America/New_York".to_owned(),
            name: "New York".to_owned(),
            utc_offset: -5 * 3600,
        },
        Timezone {
            id: "America/Sao_Paulo".to_owned(),
            name: "Sao Paulo".to_owned(),
            utc_offset: -3 * 3600,
        },
        Timezone {
            id: "Europe/London".to_owned(),
            name: "London".to_owned(),
            utc_offset: 0,
        },
        Timezone {
            id: "Europe/Berlin".to_owned(),
            name: "Berlin".to_owned(),
            utc_offset: 3600,
        },
        Timezone {
            id: "Europe/Kyiv".to_owned(),
            name: "Kyiv".to_owned(),
            utc_offset: 2 * 3600,
        },
        Timezone {
            id: "Asia/Dubai".to_owned(),
            name: "Dubai".to_owned(),
            utc_offset: 4 * 3600,
        },
        Timezone {
            id: "Asia/Kolkata".to_owned(),
            name: "Kolkata".to_owned(),
            utc_offset: 5 * 3600 + 1800,
        },
        Timezone {
            id: "Asia/Singapore".to_owned(),
            name: "Singapore".to_owned(),
            utc_offset: 8 * 3600,
        },
        Timezone {
            id: "Asia/Tokyo".to_owned(),
            name: "Tokyo".to_owned(),
            utc_offset: 9 * 3600,
        },
        Timezone {
            id: "Australia/Sydney".to_owned(),
            name: "Sydney".to_owned(),
            utc_offset: 10 * 3600,
        },
    ]
}

fn default_users() -> Vec<BusinessUserRef> {
    vec![
        BusinessUserRef {
            id: 2001,
            name: "Client Desk A".to_owned(),
            username: "client_desk_a".to_owned(),
            avatar_rgb: [0x45, 0x99, 0xE8],
            is_contact: true,
        },
        BusinessUserRef {
            id: 2002,
            name: "Partner Studio B".to_owned(),
            username: "partner_studio_b".to_owned(),
            avatar_rgb: [0x3B, 0xB5, 0x66],
            is_contact: true,
        },
        BusinessUserRef {
            id: 2003,
            name: "Supplier Hub C".to_owned(),
            username: "supplier_hub_c".to_owned(),
            avatar_rgb: [0xE6, 0x7E, 0x22],
            is_contact: true,
        },
        BusinessUserRef {
            id: 2004,
            name: "Inquiry Lead D".to_owned(),
            username: "inquiry_lead_d".to_owned(),
            avatar_rgb: [0x9B, 0x59, 0xB6],
            is_contact: false,
        },
        BusinessUserRef {
            id: 2005,
            name: "Workshop Guest E".to_owned(),
            username: "workshop_guest_e".to_owned(),
            avatar_rgb: [0x14, 0xB8, 0xA6],
            is_contact: false,
        },
    ]
}

fn default_bots() -> Vec<BusinessBotCandidate> {
    vec![
        BusinessBotCandidate {
            id: 5001,
            name: "Desk Assist Bot".to_owned(),
            username: "DeskAssistBot".to_owned(),
            about: "Automated FAQ routing and ticket intake for business accounts.".to_owned(),
            supports_business: true,
            avatar_rgb: [0x0E, 0xA5, 0xE9],
        },
        BusinessBotCandidate {
            id: 5002,
            name: "Booking Flow Bot".to_owned(),
            username: "BookingFlowBot".to_owned(),
            about: "Calendar slot booking and appointment confirmations.".to_owned(),
            supports_business: true,
            avatar_rgb: [0x22, 0xC5, 0x5E],
        },
        BusinessBotCandidate {
            id: 5003,
            name: "Order Track Bot".to_owned(),
            username: "OrderTrackBot".to_owned(),
            about: "Order status lookups and shipping updates in private chats.".to_owned(),
            supports_business: true,
            avatar_rgb: [0x8B, 0x5C, 0xF6],
        },
        BusinessBotCandidate {
            id: 5004,
            name: "Basic Ping Bot".to_owned(),
            username: "BasicPingBot".to_owned(),
            about: "Legacy echo bot without Telegram Business support.".to_owned(),
            supports_business: false,
            avatar_rgb: [0x94, 0xA3, 0xB8],
        },
    ]
}

fn default_intro_stickers() -> Vec<IntroStickerOption> {
    vec![
        IntroStickerOption {
            id: 9001,
            name: "Waving Star".to_owned(),
            art_kind: "star".to_owned(),
            accent_rgb: [0xF5, 0xA6, 0x23],
        },
        IntroStickerOption {
            id: 9002,
            name: "Studio Spark".to_owned(),
            art_kind: "spark".to_owned(),
            accent_rgb: [0x6B, 0x67, 0xF7],
        },
        IntroStickerOption {
            id: 9003,
            name: "Warm Cup".to_owned(),
            art_kind: "cup".to_owned(),
            accent_rgb: [0x14, 0xB8, 0xA6],
        },
        IntroStickerOption {
            id: 9004,
            name: "Launch Rocket".to_owned(),
            art_kind: "rocket".to_owned(),
            accent_rgb: [0xEC, 0x48, 0x99],
        },
        IntroStickerOption {
            id: 9005,
            name: "Open Sign".to_owned(),
            art_kind: "sign".to_owned(),
            accent_rgb: [0x2E, 0xA6, 0x52],
        },
        IntroStickerOption {
            id: 9006,
            name: "Crown Crest".to_owned(),
            art_kind: "crown".to_owned(),
            accent_rgb: [0x9D, 0x5B, 0xE5],
        },
    ]
}

fn default_emoji_statuses() -> Vec<EmojiStatusItem> {
    vec![
        EmojiStatusItem {
            id: 7001,
            name: "Working".to_owned(),
            glyph: "WRK".to_owned(),
            category: EmojiStatusCategory::Recent,
            color_rgb: [0x2A, 0x9D, 0xF4],
            accent_rgb: [0x54, 0xC3, 0xF3],
            is_collectible: false,
            collectible_slug: None,
        },
        EmojiStatusItem {
            id: 7002,
            name: "Studio Open".to_owned(),
            glyph: "OPN".to_owned(),
            category: EmojiStatusCategory::Recent,
            color_rgb: [0x2E, 0xA6, 0x52],
            accent_rgb: [0x68, 0xD3, 0x91],
            is_collectible: false,
            collectible_slug: None,
        },
        EmojiStatusItem {
            id: 7003,
            name: "Coffee Break".to_owned(),
            glyph: "CAF".to_owned(),
            category: EmojiStatusCategory::Recent,
            color_rgb: [0xD9, 0x77, 0x06],
            accent_rgb: [0xFB, 0xBF, 0x24],
            is_collectible: false,
            collectible_slug: None,
        },
        EmojiStatusItem {
            id: 7004,
            name: "On Vacation".to_owned(),
            glyph: "SUN".to_owned(),
            category: EmojiStatusCategory::Default,
            color_rgb: [0xF5, 0x9E, 0x0B],
            accent_rgb: [0xFD, 0xE6, 0x8A],
            is_collectible: false,
            collectible_slug: None,
        },
        EmojiStatusItem {
            id: 7005,
            name: "Deep Focus".to_owned(),
            glyph: "FOC".to_owned(),
            category: EmojiStatusCategory::Default,
            color_rgb: [0x6B, 0x67, 0xF7],
            accent_rgb: [0xA7, 0x8B, 0xFA],
            is_collectible: false,
            collectible_slug: None,
        },
        EmojiStatusItem {
            id: 7006,
            name: "Do Not Disturb".to_owned(),
            glyph: "DND".to_owned(),
            category: EmojiStatusCategory::Default,
            color_rgb: [0xDC, 0x26, 0x26],
            accent_rgb: [0xF8, 0x71, 0x71],
            is_collectible: false,
            collectible_slug: None,
        },
        EmojiStatusItem {
            id: 7007,
            name: "Shipping".to_owned(),
            glyph: "SHP".to_owned(),
            category: EmojiStatusCategory::Default,
            color_rgb: [0x0E, 0xA5, 0xE9],
            accent_rgb: [0x7D, 0xD3, 0xFC],
            is_collectible: false,
            collectible_slug: None,
        },
        EmojiStatusItem {
            id: 7008,
            name: "Accent Star".to_owned(),
            glyph: "STR".to_owned(),
            category: EmojiStatusCategory::Colored,
            color_rgb: [0x40, 0xA7, 0xE3],
            accent_rgb: [0x93, 0xC5, 0xFD],
            is_collectible: false,
            collectible_slug: None,
        },
        EmojiStatusItem {
            id: 7009,
            name: "Accent Bolt".to_owned(),
            glyph: "BLT".to_owned(),
            category: EmojiStatusCategory::Colored,
            color_rgb: [0x9D, 0x5B, 0xE5],
            accent_rgb: [0xD8, 0xB4, 0xFE],
            is_collectible: false,
            collectible_slug: None,
        },
        EmojiStatusItem {
            id: 7010,
            name: "Accent Check".to_owned(),
            glyph: "CHK".to_owned(),
            category: EmojiStatusCategory::Colored,
            color_rgb: [0x14, 0xB8, 0xA6],
            accent_rgb: [0x5E, 0xEA, 0xD4],
            is_collectible: false,
            collectible_slug: None,
        },
        EmojiStatusItem {
            id: 7011,
            name: "Prism Crown".to_owned(),
            glyph: "CRN".to_owned(),
            category: EmojiStatusCategory::Collectibles,
            color_rgb: [0x7C, 0x3A, 0xED],
            accent_rgb: [0xEC, 0x48, 0x99],
            is_collectible: true,
            collectible_slug: Some("PrismCrown-104".to_owned()),
        },
        EmojiStatusItem {
            id: 7012,
            name: "Emerald Gem".to_owned(),
            glyph: "GEM".to_owned(),
            category: EmojiStatusCategory::Collectibles,
            color_rgb: [0x05, 0x96, 0x69],
            accent_rgb: [0x34, 0xD3, 0x99],
            is_collectible: true,
            collectible_slug: Some("EmeraldGem-42".to_owned()),
        },
    ]
}

fn default_premium_plans() -> Vec<PremiumPlanOption> {
    vec![
        PremiumPlanOption {
            months: 12,
            label: "Annual".to_owned(),
            discount_badge: "-33%".to_owned(),
            price_per_month: "$2.99".to_owned(),
            total_price: "$35.88".to_owned(),
            original_total: "$53.88".to_owned(),
        },
        PremiumPlanOption {
            months: 6,
            label: "6 Months".to_owned(),
            discount_badge: "-18%".to_owned(),
            price_per_month: "$3.69".to_owned(),
            total_price: "$22.14".to_owned(),
            original_total: "$26.94".to_owned(),
        },
        PremiumPlanOption {
            months: 1,
            label: "Monthly".to_owned(),
            discount_badge: String::new(),
            price_per_month: "$4.49".to_owned(),
            total_price: "$4.49".to_owned(),
            original_total: String::new(),
        },
    ]
}

fn default_tier_comparison() -> Vec<PremiumTierComparisonRow> {
    vec![
        PremiumTierComparisonRow {
            feature: "File Upload Size".to_owned(),
            free_value: "2 GB".to_owned(),
            premium_value: "4 GB".to_owned(),
        },
        PremiumTierComparisonRow {
            feature: "Channels & Supergroups".to_owned(),
            free_value: "500".to_owned(),
            premium_value: "1,000".to_owned(),
        },
        PremiumTierComparisonRow {
            feature: "Chat Folders / Chats each".to_owned(),
            free_value: "10 / 100".to_owned(),
            premium_value: "30 / 200".to_owned(),
        },
        PremiumTierComparisonRow {
            feature: "Pinned Chats (Main / Saved)".to_owned(),
            free_value: "5 / 5".to_owned(),
            premium_value: "10 / 100".to_owned(),
        },
        PremiumTierComparisonRow {
            feature: "Saved GIFs / Fav Stickers".to_owned(),
            free_value: "200 / 5".to_owned(),
            premium_value: "400 / 10".to_owned(),
        },
        PremiumTierComparisonRow {
            feature: "Bio / Media Caption".to_owned(),
            free_value: "70 / 1,024".to_owned(),
            premium_value: "140 / 2,048".to_owned(),
        },
        PremiumTierComparisonRow {
            feature: "Telegram Business Suite".to_owned(),
            free_value: "Preview".to_owned(),
            premium_value: "Included".to_owned(),
        },
        PremiumTierComparisonRow {
            feature: "Emoji Status & Profile Badge".to_owned(),
            free_value: "—".to_owned(),
            premium_value: "Unlimited".to_owned(),
        },
    ]
}

fn default_premium_features() -> Vec<PremiumFeatureEntry> {
    vec![
        PremiumFeatureEntry {
            key: "stories".to_owned(),
            title: "Stories".to_owned(),
            description: "Priority order, stealth mode, permanent view history, and longer captions.".to_owned(),
            preview_caption: "Static preview: Animated ring glow, 2,048-char captions, and stealth viewing badge.".to_owned(),
            preview_kind: "stories".to_owned(),
            icon_glyph: "ST",
            icon_rgb: [0x2A, 0x9D, 0xF4],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "more_upload".to_owned(),
            title: "4 GB Uploads".to_owned(),
            description: "Send media and documents up to 4 GB each with unlimited cloud storage.".to_owned(),
            preview_caption: "Static preview: 4.0 GB archive transfer bar running at maximum bandwidth.".to_owned(),
            preview_kind: "upload".to_owned(),
            icon_glyph: "4G",
            icon_rgb: [0x4F, 0x46, 0xE5],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "double_limits".to_owned(),
            title: "Doubled Limits".to_owned(),
            description: "Up to 1,000 channels, 30 folders, 10 pins, 20 public links, 400 GIFs, and 4 accounts.".to_owned(),
            preview_caption: "Static preview: Free vs Premium 2x split gauge across folders, channels, and pins.".to_owned(),
            preview_kind: "limits".to_owned(),
            icon_glyph: "2X",
            icon_rgb: [0x7C, 0x3A, 0xED],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "last_seen".to_owned(),
            title: "Last Seen & Read Times".to_owned(),
            description: "See others' last seen and read timestamps even while hiding your own.".to_owned(),
            preview_caption: "Static preview: Dual-check read receipt card with timestamp reveal toggle.".to_owned(),
            preview_kind: "privacy".to_owned(),
            icon_glyph: "LS",
            icon_rgb: [0x0E, 0xA5, 0xE9],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "voice_to_text".to_owned(),
            title: "Voice-to-Text Conversion".to_owned(),
            description: "Transcribe any voice or video message into readable text in one click.".to_owned(),
            preview_caption: "Static preview: Waveform bubble expanding into an instant text transcript.".to_owned(),
            preview_kind: "voice".to_owned(),
            icon_glyph: "AA",
            icon_rgb: [0x14, 0xB8, 0xA6],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "faster_download".to_owned(),
            title: "Faster Download Speed".to_owned(),
            description: "Download media and files at the fastest speed your connection supports.".to_owned(),
            preview_caption: "Static preview: Speedometer dial pinned to full throughput without rate caps.".to_owned(),
            preview_kind: "speed".to_owned(),
            icon_glyph: "HI",
            icon_rgb: [0x22, 0xC5, 0x5E],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "translations".to_owned(),
            title: "Real-Time Chat Translation".to_owned(),
            description: "Translate entire chats, groups, and channels automatically as you scroll.".to_owned(),
            preview_caption: "Static preview: Top translation bar toggling incoming bubbles seamlessly.".to_owned(),
            preview_kind: "translate".to_owned(),
            icon_glyph: "TR",
            icon_rgb: [0x02, 0x84, 0xC7],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "animated_emoji".to_owned(),
            title: "Custom Animated Emoji".to_owned(),
            description: "Use thousands of custom emoji packs inside messages, captions, and polls.".to_owned(),
            preview_caption: "Static preview: Inline custom emoji tiles shimmering inside a chat bubble.".to_owned(),
            preview_kind: "emoji".to_owned(),
            icon_glyph: "EM",
            icon_rgb: [0xF5, 0x9E, 0x0B],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "emoji_status".to_owned(),
            title: "Emoji Status".to_owned(),
            description: "Display a custom animated or collectible icon next to your name.".to_owned(),
            preview_caption: "Static preview: Profile header badge morphing across Work, Focus, and Collectible icons.".to_owned(),
            preview_kind: "status".to_owned(),
            icon_glyph: "ES",
            icon_rgb: [0x9D, 0x5B, 0xE5],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "saved_tags".to_owned(),
            title: "Tags for Saved Messages".to_owned(),
            description: "Organize Saved Messages with custom emoji tags for instant filtering.".to_owned(),
            preview_caption: "Static preview: Color-coded tag strip filtering Saved Messages by topic.".to_owned(),
            preview_kind: "tags".to_owned(),
            icon_glyph: "TG",
            icon_rgb: [0xEC, 0x48, 0x99],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "wallpapers".to_owned(),
            title: "Wallpapers for Both Sides".to_owned(),
            description: "Set a custom chat wallpaper for any 1-on-1 conversation for both participants.".to_owned(),
            preview_caption: "Static preview: Patterned chat theme synchronized across both sides of a dialog.".to_owned(),
            preview_kind: "wallpaper".to_owned(),
            icon_glyph: "WP",
            icon_rgb: [0x63, 0x66, 0xF1],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "profile_badge".to_owned(),
            title: "Profile Badge".to_owned(),
            description: "Show a Premium star badge next to your name in chats and member lists.".to_owned(),
            preview_caption: "Static preview: Gradient 5-point Premium star orbiting beside the account title.".to_owned(),
            preview_kind: "badge".to_owned(),
            icon_glyph: "★",
            icon_rgb: [0x8B, 0x5C, 0xF6],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "message_privacy".to_owned(),
            title: "Message Privacy Controls".to_owned(),
            description: "Restrict incoming private messages to Contacts and Premium users.".to_owned(),
            preview_caption: "Static preview: Shield icon blocking unsolicited messages from unknown accounts.".to_owned(),
            preview_kind: "privacy".to_owned(),
            icon_glyph: "PR",
            icon_rgb: [0x10, 0xB9, 0x81],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "advanced_chat_management".to_owned(),
            title: "Advanced Chat Management".to_owned(),
            description: "Choose a default folder, auto-archive and mute new chats from non-contacts.".to_owned(),
            preview_caption: "Static preview: Default folder pill and automatic archive rule toggles.".to_owned(),
            preview_kind: "management".to_owned(),
            icon_glyph: "CM",
            icon_rgb: [0x3B, 0x82, 0xF6],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "no_ads".to_owned(),
            title: "No Sponsored Ads".to_owned(),
            description: "Hide sponsored messages in large public broadcast channels.".to_owned(),
            preview_caption: "Static preview: Channel feed displaying uninterrupted posts with ads muted.".to_owned(),
            preview_kind: "noads".to_owned(),
            icon_glyph: "AD",
            icon_rgb: [0xEF, 0x44, 0x44],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "infinite_reactions".to_owned(),
            title: "Infinite Reactions".to_owned(),
            description: "React with up to 3 custom emoji per message across chats and channels.".to_owned(),
            preview_caption: "Static preview: Floating reaction bar stacking three animated emoji chips.".to_owned(),
            preview_kind: "reactions".to_owned(),
            icon_glyph: "RX",
            icon_rgb: [0xE0, 0x5B, 0x9E],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "animated_userpics".to_owned(),
            title: "Animated Profile Pictures".to_owned(),
            description: "Video avatars play smoothly throughout the chat list and headers.".to_owned(),
            preview_caption: "Static preview: Looping avatar portrait frames inside the dialogs sidebar.".to_owned(),
            preview_kind: "userpic".to_owned(),
            icon_glyph: "AV",
            icon_rgb: [0xF9, 0x73, 0x16],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "premium_stickers".to_owned(),
            title: "Unique Premium Stickers".to_owned(),
            description: "Send full-screen animated stickers updated monthly by Telegram artists.".to_owned(),
            preview_caption: "Static preview: Full-width sticker celebration effect bursting across the chat.".to_owned(),
            preview_kind: "stickers".to_owned(),
            icon_glyph: "SK",
            icon_rgb: [0xA8, 0x55, 0xF7],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "business".to_owned(),
            title: "Telegram Business".to_owned(),
            description: "Opening hours, location, quick replies, greeting & away messages, chat links, and bots.".to_owned(),
            preview_caption: "Static preview: Complete Telegram Business toolkit unlocked with your Premium plan.".to_owned(),
            preview_kind: "business".to_owned(),
            icon_glyph: "BZ",
            icon_rgb: [0x0D, 0x94, 0x88],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "effects".to_owned(),
            title: "Message Effects".to_owned(),
            description: "Attach full-screen animated effects when recipients open your message.".to_owned(),
            preview_caption: "Static preview: Confetti and starburst particles triggered on bubble delivery.".to_owned(),
            preview_kind: "effects".to_owned(),
            icon_glyph: "FX",
            icon_rgb: [0xD9, 0x46, 0xEF],
            is_new: false,
        },
        PremiumFeatureEntry {
            key: "ai_compose".to_owned(),
            title: "AI Compose & Tone Presets".to_owned(),
            description: "Save up to 20 writing tone presets and polish replies before sending.".to_owned(),
            preview_caption: "Static preview: Tone selector bar (Concise, Warm, Formal) refining a draft.".to_owned(),
            preview_kind: "ai".to_owned(),
            icon_glyph: "AI",
            icon_rgb: [0x6B, 0x67, 0xF7],
            is_new: true,
        },
        PremiumFeatureEntry {
            key: "rich_formatting".to_owned(),
            title: "Rich Message Formatting".to_owned(),
            description: "Extended formatting blocks, collapsible quotes, and styled headers.".to_owned(),
            preview_caption: "Static preview: Collapsible callout block and syntax-highlighted snippet card.".to_owned(),
            preview_kind: "rich".to_owned(),
            icon_glyph: "RF",
            icon_rgb: [0xE0, 0x5B, 0x9E],
            is_new: true,
        },
    ]
}

fn default_inactive_channels() -> Vec<InactiveChannelEntry> {
    vec![
        InactiveChannelEntry {
            id: 8001,
            title: "Archived Design Feed".to_owned(),
            is_broadcast: true,
            inactive_since: "channel • inactive for 11 months".to_owned(),
            avatar_rgb: [0x64, 0x74, 0x8B],
        },
        InactiveChannelEntry {
            id: 8002,
            title: "Old Sprint Planning".to_owned(),
            is_broadcast: false,
            inactive_since: "group • inactive for 8 months".to_owned(),
            avatar_rgb: [0x0E, 0xA5, 0xE9],
        },
        InactiveChannelEntry {
            id: 8003,
            title: "Conference Updates 2024".to_owned(),
            is_broadcast: true,
            inactive_since: "channel • inactive for 1 year".to_owned(),
            avatar_rgb: [0x8B, 0x5C, 0xF6],
        },
    ]
}

fn default_public_link_channels() -> Vec<PublicLinkChannelEntry> {
    vec![
        PublicLinkChannelEntry {
            id: 8101,
            title: "Northwind Announcements".to_owned(),
            public_link: "t.me/northwind_news".to_owned(),
            avatar_rgb: [0x2A, 0x9D, 0xF4],
        },
        PublicLinkChannelEntry {
            id: 8102,
            title: "Northwind Archive 2023".to_owned(),
            public_link: "t.me/northwind_2023".to_owned(),
            avatar_rgb: [0x14, 0xB8, 0xA6],
        },
        PublicLinkChannelEntry {
            id: 8103,
            title: "Workshop Sandbox".to_owned(),
            public_link: "t.me/northwind_sandbox".to_owned(),
            avatar_rgb: [0xF5, 0x9E, 0x0B],
        },
    ]
}
