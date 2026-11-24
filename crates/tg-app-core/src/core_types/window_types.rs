//! Window layout constants, navigation enums, slide animation, and overlay request DTOs
//! shared across `tg-app-core`, UI crates, `tg-app-window`, and `tg-app`.

use std::time::{Duration, Instant};

use crate::stand_in::types::PeerId;

/// Minimum window width (`st::windowMinWidth`).
pub const WINDOW_MIN_WIDTH: i32 = 380;
/// Minimum window height (`st::windowMinHeight`).
pub const WINDOW_MIN_HEIGHT: i32 = 480;
/// Default window width (`st::windowDefaultWidth`).
pub const WINDOW_DEFAULT_WIDTH: i32 = 800;
/// Default window height (`st::windowDefaultHeight`).
pub const WINDOW_DEFAULT_HEIGHT: i32 = 600;
/// Wide default window width (`st::windowBigDefaultWidth`).
pub const WINDOW_BIG_DEFAULT_WIDTH: i32 = 1024;
/// Wide default window height (`st::windowBigDefaultHeight`).
pub const WINDOW_BIG_DEFAULT_HEIGHT: i32 = 768;

/// Minimum dialogs column width when expanded (`st::columnMinimalWidthLeft`).
pub const COLUMN_MIN_WIDTH_LEFT: i32 = 260;
/// Maximum dialogs column width (`st::columnMaximalWidthLeft`).
pub const COLUMN_MAX_WIDTH_LEFT: i32 = 540;
/// Minimum main history/section column width (`st::columnMinimalWidthMain`).
pub const COLUMN_MIN_WIDTH_MAIN: i32 = 380;
/// Minimum third (info/tabbed) column width (`st::columnMinimalWidthThird`).
pub const COLUMN_MIN_WIDTH_THIRD: i32 = 292;
/// Maximum third (info/tabbed) column width (`st::columnMaximalWidthThird`).
pub const COLUMN_MAX_WIDTH_THIRD: i32 = 392;

/// Chat width threshold for wide message layout (`st::adaptiveChatWideWidth`).
pub const ADAPTIVE_CHAT_WIDE_WIDTH: i32 = 880;
/// Vertical chat folders sidebar width (`st::windowFiltersWidth`).
pub const WINDOW_FILTERS_WIDTH: i32 = 72;
/// Compact avatar-only dialogs column width: `padding.left (10) + photoSize (46) + padding.left (10)`.
pub const DIALOGS_SMALL_COLUMN_WIDTH: i32 = 66;
/// Resizable splitter hit-test width (`st::historyResizeWidth`).
pub const HISTORY_RESIZE_WIDTH: i32 = 6;
/// Default dialogs width ratio (`Core::Settings::kDefaultDialogsWidthRatio = 5. / 14`).
pub const DEFAULT_DIALOGS_WIDTH_RATIO: f64 = 5.0 / 14.0;
/// Default third column width (`Core::Settings::kDefaultThirdColumnWidth = 0`, clamped to min).
pub const DEFAULT_THIRD_COLUMN_WIDTH: i32 = 0;

/// Window column mode (`Window::Adaptive::WindowLayout`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowLayout {
    /// Narrow single-column mode: shows either dialogs OR the active main section/history.
    OneColumn,
    /// Two-column mode: dialogs on the left, main history/section on the right.
    #[default]
    Normal,
    /// Three-column mode: dialogs | main history/section | third info/tabbed column.
    ThreeColumn,
}

/// Chat bubble layout mode (`Window::Adaptive::ChatLayout`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatLayout {
    /// Standard chat layout.
    #[default]
    Normal,
    /// Wide chat layout when chat width >= `ADAPTIVE_CHAT_WIDE_WIDTH`.
    Wide,
}

/// Logical column position (`Window::Column`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    /// Left column (dialogs list, or full window in `OneColumn`).
    First,
    /// Center/main column (history or pushed section).
    Second,
    /// Right column (peer info or tabbed selector in `ThreeColumn`).
    Third,
}

/// Computed pixel widths for one frame (`Window::SessionController::ColumnLayout`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnLayout {
    /// Width of the left vertical folders sidebar (`0` or `WINDOW_FILTERS_WIDTH`).
    pub filters_width: i32,
    /// Remaining body width (`window_width - filters_width`).
    pub body_width: i32,
    /// Effective dialogs column width (may be `DIALOGS_SMALL_COLUMN_WIDTH` in compact mode).
    pub dialogs_width: i32,
    /// Effective center/chat column width.
    pub chat_width: i32,
    /// Effective third column width (`0` unless `WindowLayout::ThreeColumn`).
    pub third_width: i32,
    /// Chosen adaptive window layout.
    pub window_layout: WindowLayout,
    /// Chosen chat layout (`Wide` when `chat_width >= ADAPTIVE_CHAT_WIDE_WIDTH`).
    pub chat_layout: ChatLayout,
    /// `true` when the dialogs column is collapsed to the compact 66px avatar rail.
    pub dialogs_small_column: bool,
}

/// Standard section slide animation duration (`st::slideDuration` / `st::introSlideDuration`).
pub const SLIDE_DURATION_MS: u64 = 200;

/// Direction of a navigation transition (`Window::SectionShow::Way`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SectionWay {
    /// Push current section onto the back stack and slide in from the right.
    #[default]
    Forward,
    /// Pop / return backward and slide in from the left.
    Backward,
    /// Clear the entire back stack and show the target section at root.
    ClearStack,
}

/// Parameters controlling how a section or history is shown (`Window::SectionShow`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionShow {
    /// Navigation stack semantics.
    pub way: SectionWay,
    /// Whether the transition should animate.
    pub animated: bool,
    /// Prefer placing this section in the third column when `ThreeColumn` is active.
    pub third_column: bool,
    /// When `true`, avoids pushing duplicate adjacent entries of the same section type.
    pub drop_same_from_stack: bool,
    /// When `false` (default), navigating forward to a peer already in the stack unwinds to it.
    pub allow_duplicate_in_stack: bool,
    /// Slide vertically from bottom instead of horizontally.
    pub slide_from_bottom: bool,
}

impl Default for SectionShow {
    fn default() -> Self {
        Self::forward()
    }
}

impl SectionShow {
    /// Standard forward animated push onto the stack.
    #[must_use]
    pub const fn forward() -> Self {
        Self {
            way: SectionWay::Forward,
            animated: true,
            third_column: false,
            drop_same_from_stack: false,
            allow_duplicate_in_stack: false,
            slide_from_bottom: false,
        }
    }

    /// Standard clear-stack navigation (e.g. clicking a chat in the dialogs list).
    #[must_use]
    pub const fn clear_stack() -> Self {
        Self {
            way: SectionWay::ClearStack,
            animated: true,
            third_column: false,
            drop_same_from_stack: false,
            allow_duplicate_in_stack: false,
            slide_from_bottom: false,
        }
    }

    /// Instant non-animated clear-stack navigation.
    #[must_use]
    pub const fn instant_clear_stack() -> Self {
        Self {
            way: SectionWay::ClearStack,
            animated: false,
            third_column: false,
            drop_same_from_stack: false,
            allow_duplicate_in_stack: false,
            slide_from_bottom: false,
        }
    }

    /// Returns a copy with `way` replaced.
    #[must_use]
    pub const fn with_way(mut self, way: SectionWay) -> Self {
        self.way = way;
        self
    }

    /// Returns a copy targeting the third column.
    #[must_use]
    pub const fn with_third_column(mut self) -> Self {
        self.third_column = true;
        self
    }

    /// Returns a copy with `animated` overridden.
    #[must_use]
    pub const fn with_animated(mut self, animated: bool) -> Self {
        self.animated = animated;
        self
    }
}

/// Active tab inside a peer Info section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InfoTab {
    /// Profile overview (bio, username, notifications).
    #[default]
    Overview,
    /// Shared media placeholder tab.
    Media,
    /// Shared files placeholder tab.
    Files,
    /// Shared links placeholder tab.
    Links,
    /// Global Downloads Manager (`InfoSection::Downloads`).
    Downloads,
}

/// Sub-page within the Settings section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsPage {
    /// Root settings list.
    #[default]
    Main,
    /// Account & profile details.
    Account,
    /// Notifications & sounds placeholder.
    Notifications,
    /// Privacy, security, and local passcode lock setup.
    PrivacySecurity,
    /// Chat appearance, Night Mode, and folders sidebar.
    ChatSettings,
    /// Connection status & proxy simulator.
    Connection,
    /// Advanced layout diagnostics.
    Advanced,
}

/// Slide direction (`Window::SlideDirection`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlideDirection {
    /// Incoming content slides in from the left (going Back).
    FromLeft,
    /// Incoming content slides in from the right (going Forward).
    FromRight,
    /// Incoming content slides up from the bottom.
    FromBottom,
}

/// Lightweight wall-clock slide animation matching `Window::SlideAnimation`.
#[derive(Debug, Clone)]
pub struct SlideAnimation {
    direction: SlideDirection,
    started_at: Instant,
    duration: Duration,
}

impl SlideAnimation {
    /// Starts a new slide animation with the given direction.
    #[must_use]
    pub fn start(direction: SlideDirection) -> Self {
        let duration_ms = match direction {
            SlideDirection::FromBottom => SLIDE_DURATION_MS * 2,
            SlideDirection::FromLeft | SlideDirection::FromRight => SLIDE_DURATION_MS,
        };
        Self {
            direction,
            started_at: crate::clock::now(),
            duration: Duration::from_millis(duration_ms),
        }
    }

    /// Returns the direction of this transition.
    #[must_use]
    pub const fn direction(&self) -> SlideDirection {
        self.direction
    }

    /// Returns raw normalized progress `0.0..=1.0`.
    #[must_use]
    pub fn raw_progress(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.started_at);
        (elapsed.as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// Returns eased progress `0.0..=1.0` using `easeOutCirc` / `easeOutQuint` from `window_slide_animation.cpp`.
    #[must_use]
    pub fn eased_progress(&self, now: Instant) -> f32 {
        let t = self.raw_progress(now);
        match self.direction {
            SlideDirection::FromBottom => {
                let inv = 1.0 - t;
                1.0 - inv * inv * inv * inv * inv
            }
            SlideDirection::FromLeft | SlideDirection::FromRight => {
                let shifted = t - 1.0;
                (1.0 - shifted * shifted).max(0.0).sqrt()
            }
        }
    }

    /// Returns the pixel offset to apply to the incoming view given its `span` (width or height).
    #[must_use]
    pub fn incoming_offset(&self, now: Instant, span: f32) -> f32 {
        let remaining = 1.0 - self.eased_progress(now);
        match self.direction {
            SlideDirection::FromRight | SlideDirection::FromBottom => remaining * span,
            SlideDirection::FromLeft => -remaining * span,
        }
    }

    /// Returns `true` once the animation has finished.
    #[must_use]
    pub fn is_finished(&self, now: Instant) -> bool {
        self.raw_progress(now) >= 1.0
    }
}

/// Top-level screens hosted by `PaymentsScreenHost`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PaymentsScreen {
    /// Checkout invoice summary, shipping/info editor, native card form, 3-DS verification.
    #[default]
    Checkout,
    /// Telegram Stars balance, top-ups, subscriptions, history, paid media & reactions.
    Stars,
    /// Star Gifts picker grid, collections, resale marketplace, auctions, crafting.
    StarGifts,
    /// Telegram Premium promo screen with feature list and placeholder subscribe button.
    Premium,
    /// Channel Boosts, Replace Boost, Create Giveaway & Giveaway info boxes.
    BoostsGiveaways,
    /// Telegram Stars Affiliate Programs (Join & Bot Program Setup).
    Affiliate,
    /// Telegram Passport bot authorization form, password/secret flow, identity/address forms.
    Passport,
}

impl PaymentsScreen {
    /// Display title shown in the panel header.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Checkout => "Payments & Checkout",
            Self::Stars => "Telegram Stars",
            Self::StarGifts => "Star Gifts & Marketplace",
            Self::Premium => "Telegram Premium",
            Self::BoostsGiveaways => "Boosts & Giveaways",
            Self::Affiliate => "Stars Affiliate Programs",
            Self::Passport => "Telegram Passport",
        }
    }
}

/// Wallet deep-link action (`Wallet::OpenTransferLink`, `Wallet::OpenSendGramsLink`, `Wallet::OpenTonConnectLink`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalletDeepLinkAction {
    /// Open the TON Wallet panel overview (bare `tg://sendgrams` or `tc://`).
    OpenOverview,
    /// Open the Send TON form prefilled with transfer details.
    Send {
        /// Recipient TON address or target.
        to: Option<String>,
        /// Decimal TON amount string (e.g. `"1.5"`).
        amount: Option<String>,
        /// Optional plain text comment.
        comment: Option<String>,
        /// Optional bounceable flag from a parsed `ton://transfer` address.
        bounceable: Option<bool>,
    },
    /// Open TON Connect approval or pending session view for a `tc://` / `tonconnect-` link.
    TonConnect {
        /// Encoded `tonconnect-...`, `tc://...`, or query string payload.
        raw_link: String,
    },
}

/// Overlay host launch request emitted when a deep link targets Payments, Passport, Business,
/// Stories, Calls, or TON Wallet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepLinkOverlayRequest {
    /// Open the Payments / Stars / Star Gifts / Premium / Passport screen.
    OpenPayments {
        /// Target Payments screen tab.
        screen: PaymentsScreen,
        /// Optional invoice / gift / referral slug context.
        slug: Option<String>,
    },
    /// Open the Telegram Business overlay.
    OpenBusiness,
    /// Open a peer's story viewer (`t.me/<user>/s/<id>`).
    OpenStory {
        /// Target peer ID.
        peer_id: PeerId,
        /// Optional story ID (`None` for `"live"` or latest).
        story_id: Option<i32>,
    },
    /// Open or join a conference / group voice chat call (`tg://call?slug=...` or `?voicechat`).
    JoinCall {
        /// Optional target peer ID (for peer voicechat/livestream links).
        peer_id: Option<PeerId>,
        /// Optional conference slug or invite hash.
        slug_or_hash: Option<String>,
    },
    /// Open the Appearance & Theme Studio overlay for a cloud theme (`tg://addtheme?slug=...`).
    OpenTheme {
        /// Cloud theme slug.
        slug: String,
        /// Optional resolved theme title.
        title: Option<String>,
    },
    /// Open the Appearance & Theme Studio Backgrounds tab (`tg://bg?...` / `t.me/bg/...`).
    OpenBackground {
        /// Cloud wallpaper slug or hex color / gradient specification.
        slug_or_color: String,
        /// `true` when `slug_or_color` is a hex color / gradient rather than a cloud slug.
        is_color_or_gradient: bool,
        /// Whether `mode=blur` was requested in the link query.
        blur: bool,
        /// Whether `mode=motion` was requested in the link query.
        motion: bool,
        /// Optional pattern intensity (`-100..=100`).
        intensity: Option<i32>,
        /// Optional gradient rotation in degrees.
        rotation: Option<i32>,
    },
    /// Open the Shareable Theme Backdrop picker (`ShareThemePickerState`, tdesktop
    /// `info/profile/info_profile_share_theme.cpp`) for `(peer_id, peer_name)`.
    OpenShareTheme {
        /// Target peer identifier.
        peer_id: PeerId,
        /// Peer display title shown on the shareable theme preview card.
        peer_name: String,
    },
    /// Open the built-in TON Wallet overlay (`AppWalletHost`) for send or TON Connect approval.
    OpenWallet {
        /// Wallet deep-link action to dispatch.
        action: WalletDeepLinkAction,
    },
}
