//! Backend trait and view-model bundles for WP-16:
//! Star Gifts marketplace (unique gifts, collections, per-peer filtering, wear/unwear,
//! resale, auctions, crafting), giveaway creation, boost slot reassignment, and
//! Stars Affiliate Programs (StarRef).
//!
//! Port origin: `info/peer_gifts/*`, `boxes/star_gift_resale_box.cpp`,
//! `boxes/star_gift_auction_box.cpp`, `boxes/star_gift_craft_box.cpp`,
//! `info/channel_statistics/boosts/create_giveaway_box.cpp`,
//! `boxes/peers/replace_boost_box.cpp`, and `info/bot/starref/*`.

use tg_data::{CollectibleId, PeerId, TimeId, UserId};
use tg_payments::credits::{CreditTopupOption, CreditsAmount, CreditsGiveawayOption};
use tg_payments::giveaways::{
    ForChannelBoostSlots, GiveawayDraft, GiveawayLimits, PremiumGiftCodeCatalog,
};
use tg_payments::premium::{BoostPrepaidGiveaway, GiveawayInfo, MyBoostSlot};
use tg_payments::star_gift_auctions::{
    ActiveAuctionsSnapshot, GiftAuctionAcquiredGift, GiftAuctionState,
};
use tg_payments::star_gifts::{
    CanSendGiftResult, GiftCollection, PeerGiftsFilter, ResaleGiftsDescriptor, ResaleGiftsFilter,
    StarGift, UniqueGiftValue,
};
use tg_payments::starref::{ConnectedStarRefBot, StarRefProgram, SuggestedStarRefSort};

use crate::payments::backend::ReceivedGiftItem;

/// Paginated peer gifts slice with active collections and worn collectible ID.
#[derive(Debug, Clone, Default)]
pub struct PeerGiftsPage {
    /// Target peer display name.
    pub peer_name: String,
    /// Filtered & sorted saved gifts in this page.
    pub items: Vec<ReceivedGiftItem>,
    /// Next pagination offset (`""` if last page).
    pub next_offset: String,
    /// Total count reported by server.
    pub total_count: i32,
    /// Collections belonging to this peer.
    pub collections: Vec<GiftCollection>,
    /// Collectible ID currently worn as the user's emoji status, if any.
    pub worn_collectible_id: Option<CollectibleId>,
}

/// Detailed view of a single unique gift fetched by slug (`payments.getUniqueStarGift`).
#[derive(Debug, Clone)]
pub struct UniqueGiftDetailBundle {
    /// Core `StarGift` with `unique: Some(..)`.
    pub gift: StarGift,
    /// Resolved owner display name.
    pub owner_name: String,
    /// Optional valuation breakdown (`payments.getUniqueStarGiftValueInfo`).
    pub value_info: Option<UniqueGiftValue>,
    /// Whether the current user is wearing this gift as their emoji status.
    pub is_worn: bool,
}

/// Result of invoking `payments.craftStarGift`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CraftExecutionOutcome {
    /// Craft succeeded and produced a new unique collectible gift.
    Succeeded {
        /// Resulting unique gift title (e.g. `"Crystal Prism #99"`).
        result_title: String,
        /// Resulting unique gift slug.
        result_slug: String,
        /// Summary notice.
        notice: String,
    },
    /// Craft attempt failed (server emitted `updateStarGiftCraftFail`; input gifts burned).
    Failed {
        /// Summary notice.
        notice: String,
    },
}

/// Loaded giveaway creation options (`payments.getPremiumGiftCodeOptions` + `getStarsGiveawayOptions`).
#[derive(Debug, Clone, Default)]
pub struct GiveawayOptionsBundle {
    /// Target channel peer ID.
    pub channel_peer_id: PeerId,
    /// Target channel display title.
    pub channel_title: String,
    /// Parsed Premium gift code & giveaway options catalog.
    pub premium_catalog: PremiumGiftCodeCatalog,
    /// Available Stars giveaway packs.
    pub stars_options: Vec<CreditsGiveawayOption>,
    /// Prepaid giveaways already available to launch on this channel.
    pub prepaid_giveaways: Vec<BoostPrepaidGiveaway>,
    /// Server-configured giveaway limits.
    pub limits: GiveawayLimits,
}

/// Boost slot reassignment bundle (`boxes/peers/replace_boost_box.cpp`).
#[derive(Debug, Clone, Default)]
pub struct ReplaceBoostBundle {
    /// Target channel peer ID.
    pub channel_peer_id: PeerId,
    /// Target channel display title.
    pub channel_title: String,
    /// Partitioned boost slots (`free`, `already`, `other`).
    pub slots: ForChannelBoostSlots,
    /// Display titles for peers currently holding slots in `slots.other`.
    pub peer_names: Vec<(PeerId, String)>,
    /// Current Unix timestamp used for cooldown calculations.
    pub now: TimeId,
}

/// Display entry for a connected affiliate bot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedStarRefItem {
    /// Bot display name.
    pub bot_name: String,
    /// Bot username without `@`.
    pub bot_username: String,
    /// Core connected affiliate data.
    pub info: ConnectedStarRefBot,
}

/// Display entry for a suggested affiliate program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestedStarRefItem {
    /// Bot display name.
    pub bot_name: String,
    /// Bot username without `@`.
    pub bot_username: String,
    /// Core program terms.
    pub program: StarRefProgram,
}

/// Loaded overview for the Stars Affiliate Program screen (`info/bot/starref/*`).
#[derive(Debug, Clone, Default)]
pub struct StarRefOverviewBundle {
    /// Peer receiving affiliate earnings (user or channel display title).
    pub peer_name: String,
    /// Owned bot username for the Setup tab (when managing a bot's own affiliate program).
    pub managed_bot_username: String,
    /// Owned bot display name.
    pub managed_bot_name: String,
    /// Owned bot user ID.
    pub managed_bot_id: UserId,
    /// Current affiliate program configured on the owned bot, if any.
    pub managed_program: Option<StarRefProgram>,
    /// Connected affiliate programs joined by `peer_name`.
    pub connected: Vec<ConnectedStarRefItem>,
    /// Suggested affiliate programs sorted by `sort`.
    pub suggested: Vec<SuggestedStarRefItem>,
    /// Active sort order for `suggested`.
    pub sort: SuggestedStarRefSort,
}

/// Decoupled backend trait for Star Gifts marketplace, collections, auctions, crafting,
/// giveaways, boost replacement, and Stars Affiliate Programs.
///
/// Implementations MUST NOT initiate real payments.
pub trait StarGiftsMarketplaceBackend: Send {
    /// Loads a filtered/sorted page of saved Star Gifts and collections for `peer_name`
    /// (`payments.getSavedStarGifts` + `payments.getStarGiftCollections`).
    fn load_peer_gifts(
        &self,
        peer_name: &str,
        filter: &PeerGiftsFilter,
        offset: &str,
    ) -> Result<PeerGiftsPage, String>;

    /// Fetches a unique collectible gift by slug (`payments.getUniqueStarGift` and
    /// `payments.getUniqueStarGiftValueInfo`).
    fn get_unique_star_gift(&self, slug: &str) -> Result<UniqueGiftDetailBundle, String>;

    /// Upgrades a regular upgradable saved gift (`payments.upgradeStarGift`).
    fn upgrade_star_gift(
        &mut self,
        gift_id: u64,
        keep_original_details: bool,
    ) -> Result<String, String>;

    /// Transfers a unique collectible gift to `recipient` (`payments.transferStarGift`).
    fn transfer_star_gift(&mut self, gift_id: u64, recipient: &str) -> Result<String, String>;

    /// Converts a regular saved gift into Stars (`payments.convertStarGift`).
    fn convert_star_gift(&mut self, gift_id: u64) -> Result<String, String>;

    /// Shows or hides a saved gift on the user's profile (`payments.saveStarGift`).
    fn save_star_gift(&mut self, gift_id: u64, unsave: bool) -> Result<String, String>;

    /// Pins/unpins one or more saved gifts at the top of the profile
    /// (`payments.toggleStarGiftsPinnedToTop`).
    fn toggle_star_gifts_pinned_to_top(&mut self, gift_ids: &[u64]) -> Result<String, String>;

    /// Wears (`Some(id)`) or unwears (`None`) a unique collectible gift as the user's emoji status
    /// (`account.updateEmojiStatus`).
    fn wear_unique_gift(
        &mut self,
        collectible_id: Option<CollectibleId>,
        until: Option<TimeId>,
    ) -> Result<String, String>;

    /// Loads Star Gift collections for `peer_name` (`payments.getStarGiftCollections`).
    fn load_star_gift_collections(&self, peer_name: &str) -> Result<Vec<GiftCollection>, String>;

    /// Creates a new Star Gift collection (`payments.createStarGiftCollection`).
    fn create_star_gift_collection(
        &mut self,
        peer_name: &str,
        title: &str,
        gift_ids: &[u64],
    ) -> Result<GiftCollection, String>;

    /// Updates a collection's title or member gifts (`payments.updateStarGiftCollection`).
    fn update_star_gift_collection(
        &mut self,
        peer_name: &str,
        collection_id: i32,
        title: Option<&str>,
        add_gift_ids: &[u64],
        delete_gift_ids: &[u64],
    ) -> Result<GiftCollection, String>;

    /// Reorders Star Gift collections (`payments.reorderStarGiftCollections`).
    fn reorder_star_gift_collections(
        &mut self,
        peer_name: &str,
        order: &[i32],
    ) -> Result<String, String>;

    /// Deletes a Star Gift collection (`payments.deleteStarGiftCollection`).
    fn delete_star_gift_collection(
        &mut self,
        peer_name: &str,
        collection_id: i32,
    ) -> Result<String, String>;

    /// Queries resale marketplace listings (`payments.getResaleStarGifts`).
    fn load_resale_star_gifts(
        &self,
        gift_id: u64,
        filter: &ResaleGiftsFilter,
        offset: &str,
    ) -> Result<ResaleGiftsDescriptor, String>;

    /// Lists, updates, or unlists (amount == 0) a unique gift on the resale marketplace
    /// (`payments.updateStarGiftPrice`).
    fn update_star_gift_price(
        &mut self,
        gift_id: u64,
        resell_amount: CreditsAmount,
    ) -> Result<String, String>;

    /// Purchases a resale listing via `inputInvoiceStarGiftResale` (mock/fake only).
    fn buy_resale_star_gift(
        &mut self,
        slug: &str,
        recipient: &str,
        ton: bool,
        message: &str,
    ) -> Result<String, String>;

    /// Loads active auctions (`payments.getStarGiftActiveAuctions`).
    fn load_active_auctions(&self) -> Result<ActiveAuctionsSnapshot, String>;

    /// Loads state for a single Star Gift auction (`payments.getStarGiftAuctionState`).
    fn load_auction_state(&self, gift_id: u64, version: i32) -> Result<GiftAuctionState, String>;

    /// Loads gifts already acquired by the user in an auction
    /// (`payments.getStarGiftAuctionAcquiredGifts`).
    fn load_auction_acquired_gifts(
        &self,
        gift_id: u64,
    ) -> Result<Vec<GiftAuctionAcquiredGift>, String>;

    /// Places or raises a bid in a Star Gift auction (`inputInvoiceStarGiftAuctionBid`, mock/fake only).
    fn place_auction_bid(
        &mut self,
        gift_id: u64,
        bid_amount: i64,
        update_bid: bool,
        hide_name: bool,
        recipient: &str,
        message: &str,
    ) -> Result<String, String>;

    /// Loads owned unique gifts eligible for crafting (`payments.getCraftStarGifts`).
    fn load_craft_star_gifts(
        &self,
        gift_id: u64,
        offset: &str,
    ) -> Result<Vec<ReceivedGiftItem>, String>;

    /// Crafts a new unique gift from 1..=4 owned gifts (`payments.craftStarGift`).
    fn craft_star_gift(&mut self, gift_ids: &[u64]) -> Result<CraftExecutionOutcome, String>;

    /// Loads Premium gift code options and Stars giveaway options for creating a giveaway
    /// (`payments.getPremiumGiftCodeOptions` + `payments.getStarsGiveawayOptions`).
    fn load_giveaway_options(&self, channel_name: &str) -> Result<GiveawayOptionsBundle, String>;

    /// Launches a prepaid giveaway (`payments.launchPrepaidGiveaway`) or prepares the giveaway
    /// invoice (`payments.getPaymentForm` with `InputInvoicePremiumGiftCode` / `InputInvoiceStars`).
    fn create_or_launch_giveaway(&mut self, draft: &GiveawayDraft) -> Result<String, String>;

    /// Resolves giveaway info and results (`payments.getGiveawayInfo`).
    fn load_giveaway_info(&self, peer_name: &str, msg_id: i32) -> Result<GiveawayInfo, String>;

    /// Loads the user's boost slots partitioned for `channel_name` (`premium.getMyBoosts`).
    fn load_boost_slots_for_channel(
        &self,
        channel_name: &str,
    ) -> Result<ReplaceBoostBundle, String>;

    /// Applies or reassigns boost slots to `channel_name` (`premium.applyBoost`).
    fn apply_boost_slots(
        &mut self,
        channel_name: &str,
        slots: &[i32],
    ) -> Result<Vec<MyBoostSlot>, String>;

    /// Loads connected & suggested Stars affiliate programs (`payments.getConnectedStarRefBots` +
    /// `payments.getSuggestedStarRefBots`).
    fn load_starref_overview(
        &self,
        peer_name: &str,
        sort: SuggestedStarRefSort,
    ) -> Result<StarRefOverviewBundle, String>;

    /// Creates, updates, or terminates a bot's Stars affiliate program
    /// (`bots.updateStarRefProgram`).
    fn update_starref_program(
        &mut self,
        bot_username: &str,
        commission_permille: i32,
        duration_months: i32,
    ) -> Result<StarRefProgram, String>;

    /// Connects a peer to a bot's Stars affiliate program (`payments.connectStarRefBot`).
    fn connect_starref_bot(
        &mut self,
        peer_name: &str,
        bot_username: &str,
    ) -> Result<ConnectedStarRefBot, String>;

    /// Revokes or edits a connected affiliate link (`payments.editConnectedStarRefBot`).
    fn edit_connected_starref_bot(
        &mut self,
        peer_name: &str,
        link: &str,
        revoked: bool,
    ) -> Result<ConnectedStarRefBot, String>;

    /// Checks whether a catalog Star Gift can currently be sent (`payments.checkCanSendGift`).
    fn check_can_send_gift(&self, _gift_id: u64) -> Result<CanSendGiftResult, String> {
        Ok(CanSendGiftResult::Ok)
    }

    /// Loads Stars gift pack options for gifting Stars to `recipient`
    /// (`payments.getStarsGiftOptions`).
    fn load_stars_gift_options(&self, _recipient: &str) -> Result<Vec<CreditTopupOption>, String> {
        Ok(Vec::new())
    }
    /// Withdraws a unique Star Gift to the user's TON wallet (`payments.withdrawStarGift`),
    /// authenticating with the user's 2FA cloud password via `account.getTmpPassword` (SRP).
    ///
    /// Must only be called after explicit user confirmation.
    fn withdraw_star_gift(
        &mut self,
        _gift_id: u64,
        _password: &str,
        _keep_on_profile: bool,
    ) -> Result<String, String> {
        Err("Star Gift TON withdrawal is not supported by this backend.".to_owned())
    }

    /// Requests a blockchain/Fragment withdrawal URL for a unique Star Gift
    /// (`payments.getStarGiftWithdrawalUrl`), authenticating with the user's 2FA cloud password
    /// via `account.getTmpPassword` (SRP).
    ///
    /// Must only be called after explicit user confirmation.
    fn get_star_gift_withdrawal_url(
        &mut self,
        _gift_id: u64,
        _password: &str,
    ) -> Result<String, String> {
        Err("Star Gift withdrawal URL export is not supported by this backend.".to_owned())
    }

    /// Permanently deletes a saved Star Gift (`payments.deleteStarGift`).
    fn delete_star_gift(&mut self, _gift_id: u64) -> Result<String, String> {
        Err("Deleting Star Gifts is not supported by this backend.".to_owned())
    }

    /// Fetches a single saved Star Gift by its cached ID (`payments.getSavedStarGift`).
    fn get_saved_star_gift(&self, _gift_id: u64) -> Result<ReceivedGiftItem, String> {
        Err("Fetching a single saved Star Gift is not supported by this backend.".to_owned())
    }

    /// Returns whether channel/chat Star Gift notifications are currently enabled for `peer_name`.
    fn chat_star_gift_notifications_enabled(&self, _peer_name: &str) -> bool {
        false
    }
    /// Enables or disables channel/chat Star Gift notifications
    /// (`payments.toggleChatStarGiftNotifications`).
    fn toggle_chat_star_gift_notifications(
        &mut self,
        _peer_name: &str,
        _enabled: bool,
    ) -> Result<bool, String> {
        Err("Toggling channel Star Gift notifications is not supported by this backend.".to_owned())
    }
}
