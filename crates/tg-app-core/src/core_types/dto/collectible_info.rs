//! Pure Fragment collectible info DTOs (`boxes/collectible_info.rs` model).

use crate::info::values::{
    CollectibleEntityKind, detect_collectible_type, format_collectible_crypto_amount,
    format_collectible_date, format_collectible_entity, format_collectible_fiat_amount,
    format_username_link,
};
use crate::stand_in::types::PeerId;

/// Domain view-model for a Fragment collectible (`Ui::CollectibleInfo` / `fragment.collectibleInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectibleInfoData {
    /// Raw collectible entity (username without `@`, or phone with leading `+`).
    pub entity: String,
    /// Classified collectible kind (`Phone` or `Username`).
    pub kind: CollectibleEntityKind,
    /// Text copied by the primary bottom button (`https://t.me/<user>` or formatted phone).
    pub copy_text: String,
    /// Owning peer identifier.
    pub owner_peer_id: PeerId,
    /// Owning peer display name shown in the owner pill chip.
    pub owner_name: String,
    /// Owning peer avatar color index (`0..7`).
    pub owner_color_index: u8,
    /// Unix timestamp of the Fragment purchase (`purchase_date`).
    pub purchase_date: i32,
    /// Fiat ISO currency code (e.g. `"USD"`).
    pub currency: String,
    /// Fiat amount in smallest currency units (e.g. cents).
    pub amount: u64,
    /// Cryptocurrency code (typically `"TON"`).
    pub crypto_currency: String,
    /// Cryptocurrency amount in smallest units (e.g. nanoTON).
    pub crypto_amount: u64,
    /// External Fragment listing URL (`https://fragment.com/...`).
    pub url: String,
}

impl CollectibleInfoData {
    /// Constructs a [`CollectibleInfoData`] from raw `fragment.collectibleInfo` fields.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn from_fragment_fields(
        entity: impl Into<String>,
        owner_peer_id: PeerId,
        owner_name: impl Into<String>,
        owner_color_index: u8,
        purchase_date: i32,
        currency: impl Into<String>,
        amount: u64,
        crypto_currency: impl Into<String>,
        crypto_amount: u64,
        url: impl Into<String>,
    ) -> Self {
        let entity = entity.into();
        let kind = detect_collectible_type(&entity);
        let copy_text = match kind {
            CollectibleEntityKind::Phone => format_collectible_entity(kind, &entity),
            CollectibleEntityKind::Username => format_username_link(&entity),
        };
        Self {
            entity,
            kind,
            copy_text,
            owner_peer_id,
            owner_name: owner_name.into(),
            owner_color_index,
            purchase_date,
            currency: currency.into(),
            amount,
            crypto_currency: crypto_currency.into(),
            crypto_amount,
            url: url.into(),
        }
    }

    /// Returns the formatted entity (`@username` or `+888 ...`).
    #[must_use]
    pub fn formatted_entity(&self) -> String {
        format_collectible_entity(self.kind, &self.entity)
    }

    /// Returns the headline shown beneath the collectible emblem.
    #[must_use]
    pub fn headline(&self) -> String {
        let formatted = self.formatted_entity();
        match self.kind {
            CollectibleEntityKind::Phone => format!("{formatted} is an anonymous number"),
            CollectibleEntityKind::Username => format!("{formatted} is a collectible username"),
        }
    }

    /// Returns the purchase summary sentence (`"Bought on <date> for <TON> (<fiat>)."`).
    #[must_use]
    pub fn summary_line(&self) -> String {
        let date = format_collectible_date(self.purchase_date);
        let crypto = format_collectible_crypto_amount(&self.crypto_currency, self.crypto_amount);
        let fiat = format_collectible_fiat_amount(&self.currency, self.amount);
        match self.kind {
            CollectibleEntityKind::Phone => {
                format!("Someone bought this anonymous number on {date} for {crypto} ({fiat}).")
            }
            CollectibleEntityKind::Username => {
                format!("Someone bought this username on {date} for {crypto} ({fiat}).")
            }
        }
    }

    /// Returns the primary copy button label matching `collectible_info_box.cpp:247-250`.
    #[must_use]
    pub const fn copy_button_label(&self) -> &'static str {
        match self.kind {
            CollectibleEntityKind::Phone => "Copy Phone Number",
            CollectibleEntityKind::Username => "Copy Link",
        }
    }

    /// Returns `Some(&str)` if [`Self::url`] is a safe `https://` URL for read-only handoff.
    #[must_use]
    pub fn validated_fragment_url(&self) -> Option<&str> {
        let trimmed = self.url.trim();
        if trimmed.starts_with("https://") && trimmed.len() > "https://".len() {
            Some(trimmed)
        } else {
            None
        }
    }
}

/// Interactive action emitted by [`CollectibleInfoBoxState::show_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectibleInfoBodyAction {
    /// Clicked the entity headline to copy `@username` or formatted phone.
    CopyEntity(String),
    /// Clicked the primary copy button (`Copy Link` or `Copy Phone Number`).
    CopyPrimary(String),
    /// Clicked `Learn More` to open the read-only Fragment URL.
    OpenFragmentUrl(String),
}
