//! TL (`tg-tl`) <-> domain (`tg-data`) conversion layer.
//!
//! Port origin: `data/data_session.cpp` (`processUser`, `processChat`, `processMessage`),
//! `history/history_item.cpp`, `data/data_media_types.cpp`, `data/data_chat_filters.cpp`,
//! `data/data_drafts.cpp`, `api/api_updates.cpp` (`DataIsLoaded`).

pub mod dialogs;
pub mod media;
pub mod messages;
pub mod peers;

pub use dialogs::{
    apply_tl_dialog, apply_tl_dialog_in_folder, apply_tl_dialogs_response,
    apply_tl_dialogs_response_for_folder, apply_tl_peer_dialogs, apply_tl_peer_dialogs_for_folder,
    dialog_filter_from_tl, draft_from_tl, draft_to_save_request, tl_message_meta,
    typing_action_from_tl,
};
pub use media::{
    document_from_tl, geo_point_from_tl, message_action_from_tl, message_media_from_tl,
    photo_from_tl, reaction_from_tl, reaction_to_tl, reactions_from_tl, stars_amount_from_tl,
    webpage_from_tl,
};
pub use messages::{
    apply_tl_existing_message, apply_tl_message, entities_from_tl, entities_from_tl_for_text,
    entities_from_tl_with_self, entities_to_tl, entities_to_tl_for_sending, forward_info_from_tl,
    inline_button_from_tl, inline_query_peer_types_from_tl, input_reply_to_from_reply_info,
    keyboard_button_from_tl, message_from_short_chat_message, message_from_short_message,
    message_from_tl, message_peer_loaded, reply_info_from_tl, reply_markup_from_tl,
    short_chat_message_data_loaded, short_message_data_loaded, text_with_entities_from_tl,
    text_with_entities_to_tl,
};
pub use peers::{
    admin_rights_from_tl, admin_rights_to_tl, apply_tl_chat, apply_tl_chat_full, apply_tl_user,
    apply_tl_user_full, apply_tl_users_and_chats, banned_rights_from_tl, banned_rights_to_tl,
    dialog_key_from_tl, emoji_status_from_tl, input_channel_for, input_dialog_peer_for,
    input_peer_for, input_peer_to_input_channel, input_user_for, peer_id_from_input_peer,
    peer_id_from_tl, peer_notify_settings_from_tl, peer_to_tl, user_status_from_tl,
};
