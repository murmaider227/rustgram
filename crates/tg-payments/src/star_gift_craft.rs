//! Star Gift crafting model, eligibility validation, attribute probability math,
//! and Layer 233 TL request builders.
//!
//! Port of `boxes/star_gift_craft_box.h`, `boxes/star_gift_craft_box.cpp`,
//! and `main/main_app_config.cpp` (`craftAttributePermilles`).

use tg_data::{DocumentId, TimeId};
use tg_tl::{enums as tl_enums, functions as tl_fn};

use crate::star_gifts::{GIFTS_PER_PAGE, SavedStarGiftId, UniqueGift, UniqueGiftBackdrop};

/// Maximum number of gifts that can be combined in a single craft attempt.
pub const MAX_CRAFT_SLOTS: usize = 4;

/// Default attribute inheritance permilles table (`stargifts_craft_attribute_permilles`
/// fallback in `main/main_app_config.cpp:726-732`).
///
/// Indexed by `[total_gifts - 1][matching_count - 1]`.
pub const DEFAULT_CRAFT_ATTRIBUTE_PERMILLES: [&[i32]; 4] =
    [&[90], &[80, 200], &[70, 190, 460], &[60, 180, 450, 1000]];

/// Error explaining why a gift or craft selection cannot be crafted yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CraftEligibilityError {
    /// No gifts selected for crafting.
    Empty,
    /// More than [`MAX_CRAFT_SLOTS`] gifts selected.
    TooManySlots {
        /// Number of slots provided.
        provided: usize,
        /// Maximum allowed slots.
        max: usize,
    },
    /// Gift is locked from crafting until `can_craft_at`.
    TooEarly {
        /// Gift slug.
        slug: String,
        /// Earliest craft Unix timestamp.
        available_at: TimeId,
    },
    /// Gift was exported to an on-chain TON wallet and must be deposited back first.
    ExportedOnChain {
        /// Gift slug.
        slug: String,
    },
    /// Gift is already burned.
    Burned {
        /// Gift slug.
        slug: String,
    },
    /// Selected gifts do not share the same base `initial_gift_id`.
    MismatchedBaseGift {
        /// Expected base gift ID.
        expected: u64,
        /// Conflicting gift's base gift ID.
        found: u64,
    },
    /// Duplicate gift selected in multiple craft slots.
    DuplicateGift {
        /// Duplicated gift slug.
        slug: String,
    },
}

/// One unique gift placed into the crafting forge (`Ui::GiftForCraft`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CraftGiftSlot {
    /// Unique gift metadata.
    pub unique: UniqueGift,
    /// Manageable saved gift ID for `payments.craftStarGift`.
    pub manage_id: SavedStarGiftId,
}

/// Predicted attribute inheritance probability shown in the crafting preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CraftAttributeChance {
    /// Attribute display name.
    pub name: String,
    /// Probability in permille (`0..=1000`).
    pub permille: i32,
    /// Number of input gifts contributing this attribute.
    pub count: usize,
    /// Whether this entry is a backdrop (`true`) or pattern symbol (`false`).
    pub is_backdrop: bool,
    /// Backdrop color metadata when `is_backdrop == true`.
    pub backdrop: Option<UniqueGiftBackdrop>,
    /// Pattern sticker document ID when `is_backdrop == false`.
    pub pattern_document_id: Option<DocumentId>,
}

/// Aggregated crafting summary computed from the currently slotted gifts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CraftPlanSummary {
    /// Total craft success chance in permille (`0..=1000`).
    pub success_permille: i32,
    /// Formatted percentage string (e.g., `"45%"`).
    pub success_percent_text: String,
    /// Backdrop inheritance chances sorted by count descending.
    pub backdrop_chances: Vec<CraftAttributeChance>,
    /// Pattern inheritance chances sorted by count descending.
    pub pattern_chances: Vec<CraftAttributeChance>,
}

/// Formats a permille value as a rounded integer percentage (`FormatPercent` in `star_gift_craft_box.cpp`).
pub fn format_craft_percent(permille: i32) -> String {
    let rounded = (permille.max(0) + 5) / 10;
    format!("{rounded}%")
}

/// Computes the combined craft success chance in permille (`star_gift_craft_box.cpp:455-466`).
pub fn compute_craft_success_permille(slots: &[CraftGiftSlot]) -> i32 {
    let sum: i32 = slots
        .iter()
        .map(|s| s.unique.craft_chance_permille.max(0))
        .fold(0_i32, i32::saturating_add);
    sum.min(1000)
}

/// Looks up the attribute inheritance permille for `(total, count)` against an optional
/// app-config override table, falling back to [`DEFAULT_CRAFT_ATTRIBUTE_PERMILLES`].
pub fn lookup_craft_attribute_permille(
    total: usize,
    count: usize,
    custom_table: Option<&[Vec<i32>]>,
) -> i32 {
    if total == 0 || count == 0 {
        return 0;
    }
    if let Some(table) = custom_table {
        if let Some(row) = table.get(total - 1) {
            return row.get(count - 1).copied().unwrap_or(1000);
        }
        return 1000;
    }
    if let Some(row) = DEFAULT_CRAFT_ATTRIBUTE_PERMILLES.get(total - 1) {
        row.get(count - 1).copied().unwrap_or(1000)
    } else {
        1000
    }
}

/// Computes the full [`CraftPlanSummary`] (success rate + backdrop/pattern chances)
/// matching `star_gift_craft_box.cpp:1029-1126`.
pub fn compute_craft_plan(
    slots: &[CraftGiftSlot],
    custom_permilles: Option<&[Vec<i32>]>,
) -> CraftPlanSummary {
    let success_permille = compute_craft_success_permille(slots);
    let total = slots.len();
    if total == 0 {
        return CraftPlanSummary {
            success_permille: 0,
            success_percent_text: format_craft_percent(0),
            backdrop_chances: Vec::new(),
            pattern_chances: Vec::new(),
        };
    }

    let mut backdrops: Vec<(UniqueGiftBackdrop, usize)> = Vec::new();
    let mut patterns: Vec<(DocumentId, String, usize)> = Vec::new();

    for slot in slots {
        if let Some(existing) = backdrops
            .iter_mut()
            .find(|(b, _)| *b == slot.unique.backdrop)
        {
            existing.1 += 1;
        } else {
            backdrops.push((slot.unique.backdrop.clone(), 1));
        }

        let pat_doc = slot.unique.pattern.document_id;
        if let Some(existing) = patterns.iter_mut().find(|(doc, _, _)| *doc == pat_doc) {
            existing.2 += 1;
        } else {
            patterns.push((pat_doc, slot.unique.pattern.attribute.name.clone(), 1));
        }
    }

    backdrops.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    patterns.sort_by_key(|entry| std::cmp::Reverse(entry.2));

    let backdrop_chances = backdrops
        .into_iter()
        .map(|(backdrop, count)| CraftAttributeChance {
            name: backdrop.attribute.name.clone(),
            permille: lookup_craft_attribute_permille(total, count, custom_permilles),
            count,
            is_backdrop: true,
            backdrop: Some(backdrop),
            pattern_document_id: None,
        })
        .collect();

    let pattern_chances = patterns
        .into_iter()
        .map(|(doc_id, name, count)| CraftAttributeChance {
            name,
            permille: lookup_craft_attribute_permille(total, count, custom_permilles),
            count,
            is_backdrop: false,
            backdrop: None,
            pattern_document_id: Some(doc_id),
        })
        .collect();

    CraftPlanSummary {
        success_permille,
        success_percent_text: format_craft_percent(success_permille),
        backdrop_chances,
        pattern_chances,
    }
}

/// Validates that a single unique gift can be added to the craft forge at Unix time `now`.
pub fn validate_single_gift_for_craft(
    unique: &UniqueGift,
    now: TimeId,
) -> Result<(), CraftEligibilityError> {
    if unique.burned {
        return Err(CraftEligibilityError::Burned {
            slug: unique.slug.clone(),
        });
    }
    if !unique.gift_address.is_empty() {
        return Err(CraftEligibilityError::ExportedOnChain {
            slug: unique.slug.clone(),
        });
    }
    if unique.can_craft_at > now {
        return Err(CraftEligibilityError::TooEarly {
            slug: unique.slug.clone(),
            available_at: unique.can_craft_at,
        });
    }
    Ok(())
}

/// Validates a full set of craft slots before invoking `payments.craftStarGift`.
pub fn validate_craft_slots(
    slots: &[CraftGiftSlot],
    now: TimeId,
) -> Result<(), CraftEligibilityError> {
    if slots.is_empty() {
        return Err(CraftEligibilityError::Empty);
    }
    if slots.len() > MAX_CRAFT_SLOTS {
        return Err(CraftEligibilityError::TooManySlots {
            provided: slots.len(),
            max: MAX_CRAFT_SLOTS,
        });
    }
    let expected_base = slots[0].unique.initial_gift_id;
    let mut seen_slugs = std::collections::BTreeSet::new();
    for slot in slots {
        validate_single_gift_for_craft(&slot.unique, now)?;
        if slot.unique.initial_gift_id != expected_base {
            return Err(CraftEligibilityError::MismatchedBaseGift {
                expected: expected_base,
                found: slot.unique.initial_gift_id,
            });
        }
        if !slot.unique.slug.is_empty() && !seen_slugs.insert(slot.unique.slug.as_str()) {
            return Err(CraftEligibilityError::DuplicateGift {
                slug: slot.unique.slug.clone(),
            });
        }
    }
    Ok(())
}

/// Parses a `STARGIFT_CRAFT_TOO_EARLY_<seconds>` RPC error string (`star_gift_craft_box.cpp:1195`).
pub fn parse_craft_too_early_error(error_type: &str) -> Option<i32> {
    error_type
        .strip_prefix("STARGIFT_CRAFT_TOO_EARLY_")
        .and_then(|rest| rest.parse::<i32>().ok())
}

// ============================================================================
// TL Request Builders
// ============================================================================

/// Builds `payments.getCraftStarGifts` for fetching craft-eligible owned gifts.
pub fn build_get_craft_star_gifts(
    gift_id: u64,
    offset: impl Into<String>,
    limit: i32,
) -> tl_fn::payments::GetCraftStarGifts {
    tl_fn::payments::GetCraftStarGifts {
        gift_id: gift_id as i64,
        offset: offset.into(),
        limit: if limit > 0 { limit } else { GIFTS_PER_PAGE },
    }
}

/// Builds `payments.craftStarGift` from the selected saved gift inputs.
pub fn build_craft_star_gift(
    stargifts: Vec<tl_enums::InputSavedStarGift>,
) -> tl_fn::payments::CraftStarGift {
    tl_fn::payments::CraftStarGift {
        stargift: stargifts,
    }
}
