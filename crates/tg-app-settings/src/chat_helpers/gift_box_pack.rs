//! Gift box sticker pack resolver & ribbon model (`Stickers::GiftBoxPack`).
//!
//! Port origin: `Telegram/SourceFiles/chat_helpers/stickers_gift_box_pack.h`
//! and `Telegram/SourceFiles/chat_helpers/stickers_gift_box_pack.cpp`.

use tg_chat::{DocumentId, StickerSetId};

/// Premium gift subscription duration dividers in months (`_premium.dividers` in
/// `stickers_gift_box_pack.cpp:20`).
pub const PREMIUM_GIFT_DIVIDERS: [i32; 5] = [1, 3, 6, 12, 24];

/// TON gift amount dividers (`_ton.dividers` in `stickers_gift_box_pack.cpp:21`).
pub const TON_GIFT_DIVIDERS: [i32; 3] = [0, 10, 50];

/// Synthetic offline set ID for the Premium Gift Box sticker pack (`inputStickerSetPremiumGifts`).
pub const GIFT_BOX_PREMIUM_SET_ID: StickerSetId = StickerSetId(70_000);

/// Synthetic offline set ID for the TON Gift Box sticker pack (`inputStickerSetTonGifts`).
pub const GIFT_BOX_TON_SET_ID: StickerSetId = StickerSetId(70_100);

/// Active category shown in the gift-box sticker pack ribbon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GiftBoxPackKind {
    /// Premium subscription & Stars gift boxes (`inputStickerSetPremiumGifts`).
    #[default]
    Premium,
    /// TON gift boxes (`inputStickerSetTonGifts`).
    Ton,
}

/// One sticker entry belonging to [`GiftBoxPack`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GiftBoxSticker {
    /// Sticker document identifier.
    pub document_id: DocumentId,
    /// Representative emoji glyph (fallback when no vector/Lottie bitmap is loaded).
    pub emoji: String,
    /// Short tier caption (e.g. `"3m"`, `"12m"`, `"10 TON"`).
    pub tier_label: String,
    /// Divider threshold (`1/3/6/12/24` months or `0/10/50` TON).
    pub divider: i32,
    /// Accent RGB tint for the gift box ribbon tile.
    pub accent_rgb: [u8; 3],
}

/// Single pack storage (`GiftBoxPack::Pack` in `stickers_gift_box_pack.h:40-47`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GiftBoxPackSlot {
    /// Server sticker set ID.
    pub set_id: StickerSetId,
    /// Server sticker set access hash.
    pub access_hash: u64,
    /// Ordered divider thresholds matching `documents` indices.
    pub dividers: Vec<i32>,
    /// Resolved gift-box sticker entries indexed by divider slot.
    pub documents: Vec<Option<GiftBoxSticker>>,
}

impl GiftBoxPackSlot {
    /// Creates an empty slot with the given `dividers`.
    #[must_use]
    pub fn new(set_id: StickerSetId, dividers: &[i32]) -> Self {
        Self {
            set_id,
            access_hash: 0,
            dividers: dividers.to_vec(),
            documents: Vec::new(),
        }
    }

    /// Applies server `messages.stickerSet` pack documents using tdesktop's `alt[0] - '1'` mapping
    /// (`GiftBoxPack::applySet` in `stickers_gift_box_pack.cpp:107-147`).
    pub fn apply_set(
        &mut self,
        set_id: StickerSetId,
        access_hash: u64,
        entries: &[(DocumentId, &str, &str, [u8; 3])],
    ) {
        self.set_id = set_id;
        self.access_hash = access_hash;
        self.documents.clear();

        for &(doc_id, alt, tier_label, accent_rgb) in entries {
            let sticker = GiftBoxSticker {
                document_id: doc_id,
                emoji: alt.to_owned(),
                tier_label: tier_label.to_owned(),
                divider: self.dividers.first().copied().unwrap_or(0),
                accent_rgb,
            };
            if self.documents.is_empty() {
                self.documents.push(Some(sticker.clone()));
            }
            if let Some(first_ch) = alt.chars().next() {
                let code = first_ch as i32;
                let idx = code - ('1' as i32);
                if idx >= 0 && (idx as usize) < self.dividers.len() {
                    let uidx = idx as usize;
                    if uidx + 1 > self.documents.len() {
                        self.documents.resize(uidx + 1, None);
                    }
                    let mut mapped = sticker;
                    mapped.divider = self.dividers[uidx];
                    self.documents[uidx] = Some(mapped);
                }
            }
        }
    }

    /// Looks up the sticker matching `divider`, porting `GiftBoxPack::lookup`
    /// (`stickers_gift_box_pack.cpp:48-70`) verbatim.
    ///
    /// - When `exact == false` (Premium months), picks the nearest divider.
    /// - When `exact == true` (TON amounts), rounds down to the highest divider `<= divider`.
    #[must_use]
    pub fn lookup(&self, divider: i32, exact: bool) -> Option<&GiftBoxSticker> {
        let fallback = self.documents.first().and_then(Option::as_ref);
        let pos = self.dividers.partition_point(|&d| d < divider);
        if pos == 0 {
            return fallback;
        }
        if pos >= self.dividers.len() {
            return self
                .documents
                .iter()
                .rev()
                .find_map(Option::as_ref)
                .or(fallback);
        }
        let prev = self.dividers[pos - 1];
        let curr = self.dividers[pos];
        let index = if exact {
            if curr > divider { pos - 1 } else { pos }
        } else if (divider - prev).abs() < (divider - curr).abs() {
            pos - 1
        } else {
            pos
        };
        self.documents
            .get(index)
            .and_then(Option::as_ref)
            .or(fallback)
    }

    /// Returns all populated sticker entries in display order.
    #[must_use]
    pub fn items(&self) -> Vec<&GiftBoxSticker> {
        self.documents.iter().filter_map(Option::as_ref).collect()
    }
}

/// Port of `Stickers::GiftBoxPack` (`stickers_gift_box_pack.h` / `.cpp`).
///
/// Resolves Telegram's built-in Premium/Stars (`inputStickerSetPremiumGifts`) and TON
/// (`inputStickerSetTonGifts`) animated gift-box sticker tiers and exposes them to
/// [`crate::chat_helpers::stickers_list_widget::StickersListWidget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GiftBoxPack {
    /// Premium & Stars gift-box pack (`dividers = [1, 3, 6, 12, 24]`).
    pub premium: GiftBoxPackSlot,
    /// TON gift-box pack (`dividers = [0, 10, 50]`).
    pub ton: GiftBoxPackSlot,
}

impl Default for GiftBoxPack {
    fn default() -> Self {
        Self::demo()
    }
}

impl GiftBoxPack {
    /// Creates an empty [`GiftBoxPack`] configured with tdesktop's standard dividers.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            premium: GiftBoxPackSlot::new(GIFT_BOX_PREMIUM_SET_ID, &PREMIUM_GIFT_DIVIDERS),
            ton: GiftBoxPackSlot::new(GIFT_BOX_TON_SET_ID, &TON_GIFT_DIVIDERS),
        }
    }

    /// Builds a deterministic offline [`GiftBoxPack`] pre-populated with synthetic gift-box tiers
    /// for both Premium/Stars (`1m`, `3m`, `6m`, `12m`, `24m`) and TON (`1+`, `10+`, `50+ TON`).
    #[must_use]
    pub fn demo() -> Self {
        let mut pack = Self::empty();
        pack.premium.documents = vec![
            Some(GiftBoxSticker {
                document_id: DocumentId(70_001),
                emoji: "🎁".to_owned(),
                tier_label: "1m".to_owned(),
                divider: 1,
                accent_rgb: [0x3B, 0x82, 0xF6],
            }),
            Some(GiftBoxSticker {
                document_id: DocumentId(70_002),
                emoji: "🎀".to_owned(),
                tier_label: "3m".to_owned(),
                divider: 3,
                accent_rgb: [0x22, 0xA0, 0x6B],
            }),
            Some(GiftBoxSticker {
                document_id: DocumentId(70_003),
                emoji: "✨".to_owned(),
                tier_label: "6m".to_owned(),
                divider: 6,
                accent_rgb: [0x8B, 0x5C, 0xF6],
            }),
            Some(GiftBoxSticker {
                document_id: DocumentId(70_004),
                emoji: "👑".to_owned(),
                tier_label: "12m".to_owned(),
                divider: 12,
                accent_rgb: [0xE5, 0xA0, 0x0D],
            }),
            Some(GiftBoxSticker {
                document_id: DocumentId(70_005),
                emoji: "💎".to_owned(),
                tier_label: "24m".to_owned(),
                divider: 24,
                accent_rgb: [0xEC, 0x48, 0x99],
            }),
        ];
        pack.ton.documents = vec![
            Some(GiftBoxSticker {
                document_id: DocumentId(70_101),
                emoji: "💎".to_owned(),
                tier_label: "1 TON".to_owned(),
                divider: 0,
                accent_rgb: [0x00, 0x98, 0xEA],
            }),
            Some(GiftBoxSticker {
                document_id: DocumentId(70_102),
                emoji: "💠".to_owned(),
                tier_label: "10 TON".to_owned(),
                divider: 10,
                accent_rgb: [0x02, 0x84, 0xC7],
            }),
            Some(GiftBoxSticker {
                document_id: DocumentId(70_103),
                emoji: "🏆".to_owned(),
                tier_label: "50 TON".to_owned(),
                divider: 50,
                accent_rgb: [0x03, 0x69, 0xA1],
            }),
        ];
        pack
    }

    /// Maps a Telegram Stars gift amount to the corresponding Premium gift-box month tier
    /// (`GiftBoxPack::monthsForStars` in `stickers_gift_box_pack.cpp:30-38`).
    #[must_use]
    pub const fn months_for_stars(stars: i32) -> i32 {
        if stars <= 1000 {
            3
        } else if stars < 2500 {
            6
        } else {
            12
        }
    }

    /// Looks up the nearest Premium gift-box sticker for `months`.
    #[must_use]
    pub fn lookup(&self, months: i32) -> Option<&GiftBoxSticker> {
        self.premium.lookup(months, false)
    }

    /// Looks up the Premium gift-box sticker matching a Stars gift `stars` amount.
    #[must_use]
    pub fn lookup_for_stars(&self, stars: i32) -> Option<&GiftBoxSticker> {
        self.lookup(Self::months_for_stars(stars))
    }

    /// Looks up the TON gift-box sticker for `amount` TON (floor-exact divider match).
    #[must_use]
    pub fn ton_lookup(&self, amount: i32) -> Option<&GiftBoxSticker> {
        self.ton.lookup(amount, true)
    }

    /// Returns the active pack slot for `kind`.
    #[must_use]
    pub fn slot(&self, kind: GiftBoxPackKind) -> &GiftBoxPackSlot {
        match kind {
            GiftBoxPackKind::Premium => &self.premium,
            GiftBoxPackKind::Ton => &self.ton,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn months_for_stars_matches_tdesktop_thresholds() {
        assert_eq!(GiftBoxPack::months_for_stars(100), 3);
        assert_eq!(GiftBoxPack::months_for_stars(1000), 3);
        assert_eq!(GiftBoxPack::months_for_stars(1001), 6);
        assert_eq!(GiftBoxPack::months_for_stars(2499), 6);
        assert_eq!(GiftBoxPack::months_for_stars(2500), 12);
        assert_eq!(GiftBoxPack::months_for_stars(10_000), 12);
    }

    #[test]
    fn premium_and_ton_lookups_match_tdesktop_dividers() {
        let pack = GiftBoxPack::demo();
        assert_eq!(pack.lookup(1).map(|s| s.divider), Some(1));
        assert_eq!(pack.lookup(2).map(|s| s.divider), Some(3));
        assert_eq!(pack.lookup(4).map(|s| s.divider), Some(3));
        assert_eq!(pack.lookup(5).map(|s| s.divider), Some(6));
        assert_eq!(pack.lookup(12).map(|s| s.divider), Some(12));
        assert_eq!(pack.lookup(36).map(|s| s.divider), Some(24));

        assert_eq!(pack.ton_lookup(0).map(|s| s.divider), Some(0));
        assert_eq!(pack.ton_lookup(9).map(|s| s.divider), Some(0));
        assert_eq!(pack.ton_lookup(10).map(|s| s.divider), Some(10));
        assert_eq!(pack.ton_lookup(49).map(|s| s.divider), Some(10));
        assert_eq!(pack.ton_lookup(50).map(|s| s.divider), Some(50));
        assert_eq!(pack.ton_lookup(200).map(|s| s.divider), Some(50));
    }

    #[test]
    fn apply_set_maps_ascii_digit_alt_indices() {
        let mut slot = GiftBoxPackSlot::new(GIFT_BOX_PREMIUM_SET_ID, &PREMIUM_GIFT_DIVIDERS);
        slot.apply_set(
            StickerSetId(55),
            999,
            &[
                (DocumentId(1), "1", "1m", [1, 2, 3]),
                (DocumentId(3), "3", "6m", [4, 5, 6]),
            ],
        );
        assert_eq!(
            slot.lookup(1, false).map(|s| s.document_id),
            Some(DocumentId(1))
        );
        assert_eq!(
            slot.lookup(6, false).map(|s| s.document_id),
            Some(DocumentId(3))
        );
        // Unpopulated middle slot falls back to documents[0].
        assert_eq!(
            slot.lookup(3, false).map(|s| s.document_id),
            Some(DocumentId(1))
        );
    }
}
