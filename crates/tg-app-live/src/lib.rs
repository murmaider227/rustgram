//! `tg-app-live`: Layer-1 live MTProto/RPC session backend crate (depends ONLY on `tg-app-core`).

pub mod admin;
pub mod bots;
pub mod bridge;
pub mod business;
pub mod calls;
pub mod channel_mgmt;
pub mod chat_helpers;
pub mod chat_import;
pub mod chat_l233;
pub mod chatlists;
pub mod cloud_blob_source;
pub mod communities;
pub mod compose_ai;
pub mod compose_extras;
pub mod contacts_extras;
pub mod convert;
pub mod deep_links;
pub mod dialogs_actions;
pub mod dice_pack;
pub mod emoji_pack;
pub mod engagement;
pub mod export;
pub mod geo_map;
pub mod history_sections;
pub mod inline_bot_query;
pub mod langpack;
pub mod layer233_extras;
pub mod live_extras;
pub mod media;
pub mod media_cache;
pub mod music_send;
mod open_chat_prefetch;
pub mod paid_messages;
pub mod passkeys;
pub mod passport_upload;
pub mod payments;
pub mod polls_stats;
pub mod preview_text;
pub mod profile_extras;
pub mod profile_info_extras;
pub mod proxy;
pub mod read_metrics;
pub mod rich_message;
pub mod search_and_info;
pub mod session;
pub mod settings_extra;
pub mod settings_sync;
pub mod star_gifts;
pub mod stats;
pub mod sticker_owner;
pub mod stickers_reactions_extras;
pub mod stories;
pub mod suggestions;
pub mod summary;
pub mod themes;
pub mod toggling_media;
pub mod transcribe;
pub mod upload;
pub mod video_userpic;
pub mod wallet;
pub mod web_proxy_carrier;
pub mod webview;
pub mod widgets;
pub mod wp40_polish;
pub mod wp52_ux_extras;

#[cfg(test)]
mod tests_account_conformance;
#[cfg(test)]
mod tests_bots_miniapps;
#[cfg(test)]
mod tests_business_themes_conformance;
#[cfg(test)]
mod tests_calls_polls_extras;
#[cfg(test)]
mod tests_channel_mgmt;
#[cfg(test)]
mod tests_chat_helpers;
#[cfg(test)]
mod tests_communities;
#[cfg(test)]
mod tests_contacts_extras;
#[cfg(test)]
mod tests_engagement;
#[cfg(test)]
mod tests_fu7a_pts_pagination;
#[cfg(test)]
mod tests_history_sections;
#[cfg(test)]
mod tests_layer233_extras;
#[cfg(test)]
mod tests_live_wiring_gaps;
#[cfg(test)]
mod tests_media_cache;
#[cfg(test)]
mod tests_music_attach;
#[cfg(test)]
mod tests_payments_passport;
#[cfg(test)]
mod tests_peer_management;
#[cfg(test)]
mod tests_profile_extras;
#[cfg(test)]
mod tests_profile_info_extras;
#[cfg(test)]
mod tests_pts_feed_remaining;
#[cfg(test)]
mod tests_rich_ai_paid_msgs;
#[cfg(test)]
mod tests_rich_message;
#[cfg(test)]
mod tests_rpc_updates_routing;
#[cfg(test)]
mod tests_search_and_info;
#[cfg(test)]
mod tests_search_topics_conformance;
#[cfg(test)]
mod tests_seca;
#[cfg(test)]
mod tests_settings_extra;
#[cfg(test)]
mod tests_sticker_owner;
#[cfg(test)]
mod tests_stickers_reactions_extras;
#[cfg(test)]
mod tests_storage_wiring;
#[cfg(test)]
mod tests_stories_channels_conformance;
#[cfg(test)]
mod tests_transcribe;
#[cfg(test)]
mod tests_uip_f;
#[cfg(test)]
mod tests_unified_pts;
#[cfg(test)]
mod tests_upload;
#[cfg(test)]
mod tests_wallet_mtproto;
#[cfg(test)]
mod tests_wp52_ux_extras;
#[cfg(test)]
mod tests_wp71_live_wiring;
#[cfg(test)]
mod tests_wp74_async_and_models;
#[cfg(test)]
mod tests_wp77_data_live;

pub use bridge::{LiveBridgeState, LiveRuntimeBridge, RuntimeBridgedRpc};
pub use calls::TelegramBroadcastChunkLoader;
pub use cloud_blob_source::TelegramCloudBlobSource;
pub use history_sections::{
    DiscussionReadState, ForumTopicEdit, LiveForwardOptions, LivePollOptions,
};
pub use langpack::{LiveLangPackHandle, SetLanguageLinkOutcome};
pub use media::{
    LiveAudioDataSource, LivePlaylistProvider, ResolvedMediaLocation, TelegramMediaInvoker,
    TelegramRangeLoader, TelegramStreamChunkProvider, UpdatedFileReferences,
    document_thumb_location, download_file_bytes, download_file_bytes_async,
    download_file_bytes_with_progress_async, download_file_bytes_with_refresh_async,
    is_file_reference_error, refresh_file_reference_async,
};
pub use media_cache::LiveMediaCache;
pub use proxy::{
    LiveProxyController, ProxyIndicatorState, format_proxy_connection_status,
    map_mtproto_connection_state, proxy_indicator_state,
};
pub use session::{LiveSession, LiveSessionHandle, start_mtproto_erased_on};
pub use tg_app_core::core_types::erased_rpc::{ErasedRpcClient, RawRpcClient};
pub use webview::{ProlongWebViewHandle, tl_page_to_iv_page, tl_webpage_to_iv_page};
