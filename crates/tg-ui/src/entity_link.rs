//! Click targets of message-text entities that are not web links: hashtags, cashtags, bot
//! commands and name mentions of users without a username.
//!
//! Port origin: `HashtagClickHandler`, `CashtagClickHandler`, `BotCommandClickHandler` and
//! `MentionNameClickHandler` (`core/click_handler_types.cpp`). tdesktop gives each entity its own
//! click handler object; Rustgram's text renderers report a clicked link as a target string, so
//! both renderers (the span renderer of message bodies and the block renderer) encode these
//! entities with [`EntityLink::target`] and the owner of the text (the history widget) decodes
//! them with [`EntityLink::parse`] before the generic link router sees them.

use tg_text::EntityType;

/// Scheme of the targets made by [`EntityLink::target`]. It is not a URL scheme any server or
/// user link uses, so a real link never parses as an [`EntityLink`].
pub const ENTITY_LINK_SCHEME: &str = "tg-entity:";

/// A clicked hashtag, cashtag, bot command or name mention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityLink {
    /// `#tag` (`HashtagClickHandler`): search the tag in the current chat.
    Hashtag(String),
    /// `$TAG` (`CashtagClickHandler`): searched like a hashtag.
    Cashtag(String),
    /// `/command` or `/command@bot` (`BotCommandClickHandler`): send the command.
    BotCommand(String),
    /// A name mention of a user without a username (`MentionNameClickHandler`): open the user's
    /// profile. Holds the bare user id.
    MentionName(u64),
}

impl EntityLink {
    /// The link for an entity of `kind` with entity `data` covering `slice` of the text, or
    /// `None` when `kind` is not one of these entities (or a name mention has no user id).
    #[must_use]
    pub fn from_entity(kind: EntityType, data: &str, slice: &str) -> Option<Self> {
        match kind {
            EntityType::Hashtag => Some(Self::Hashtag(slice.to_owned())),
            EntityType::Cashtag => Some(Self::Cashtag(slice.to_owned())),
            EntityType::BotCommand => Some(Self::BotCommand(slice.to_owned())),
            EntityType::MentionName => mention_name_user_id(data).map(Self::MentionName),
            _ => None,
        }
    }

    /// The click target the text renderers report for this link.
    #[must_use]
    pub fn target(&self) -> String {
        let (kind, value) = match self {
            Self::Hashtag(tag) => ("hashtag", tag.clone()),
            Self::Cashtag(tag) => ("cashtag", tag.clone()),
            Self::BotCommand(cmd) => ("bot_command", cmd.clone()),
            Self::MentionName(user_id) => ("mention_name", user_id.to_string()),
        };
        format!("{ENTITY_LINK_SCHEME}{kind}/{value}")
    }

    /// Decodes a target made by [`Self::target`]; any other link gives `None`.
    #[must_use]
    pub fn parse(target: &str) -> Option<Self> {
        let rest = target.strip_prefix(ENTITY_LINK_SCHEME)?;
        let (kind, value) = rest.split_once('/')?;
        if value.is_empty() {
            return None;
        }
        match kind {
            "hashtag" => Some(Self::Hashtag(value.to_owned())),
            "cashtag" => Some(Self::Cashtag(value.to_owned())),
            "bot_command" => Some(Self::BotCommand(value.to_owned())),
            "mention_name" => value
                .parse()
                .ok()
                .filter(|id| *id != 0)
                .map(Self::MentionName),
            _ => None,
        }
    }
}

/// The entity link target for `kind`, or `None` when `kind` is not an [`EntityLink`] entity.
#[must_use]
pub fn entity_link_target(kind: EntityType, data: &str, slice: &str) -> Option<String> {
    EntityLink::from_entity(kind, data, slice).map(|link| link.target())
}

/// User id of a `MentionName` entity. Live messages carry the bare id; tdesktop's own format is
/// `"<userId>.<accessHash>:<selfId>"` (`TextUtilities::MentionNameDataFromFields`).
fn mention_name_user_id(data: &str) -> Option<u64> {
    let id = data.split('.').next().unwrap_or(data);
    id.parse().ok().filter(|id| *id != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_through_its_target() {
        let cases = [
            (
                EntityType::Hashtag,
                "",
                "#release",
                EntityLink::Hashtag("#release".into()),
            ),
            (
                EntityType::Cashtag,
                "",
                "$TON",
                EntityLink::Cashtag("$TON".into()),
            ),
            (
                EntityType::BotCommand,
                "",
                "/start@crew_bot",
                EntityLink::BotCommand("/start@crew_bot".into()),
            ),
            (
                EntityType::MentionName,
                "4242",
                "Lia Moss",
                EntityLink::MentionName(4242),
            ),
            (
                EntityType::MentionName,
                "4242.99:7",
                "Lia Moss",
                EntityLink::MentionName(4242),
            ),
        ];
        for (kind, data, slice, expected) in cases {
            let link = EntityLink::from_entity(kind, data, slice).expect("entity link");
            assert_eq!(link, expected);
            assert_eq!(EntityLink::parse(&link.target()), Some(expected));
        }
    }

    #[test]
    fn other_links_do_not_parse() {
        for target in [
            "https://example.org/#tag",
            "tg://resolve?domain=crew_bot",
            "#release",
            "tg-entity:hashtag/",
            "tg-entity:unknown/x",
            "tg-entity:mention_name/abc",
        ] {
            assert_eq!(EntityLink::parse(target), None, "{target}");
        }
        assert_eq!(EntityLink::from_entity(EntityType::Url, "", "x"), None);
        assert_eq!(
            EntityLink::from_entity(EntityType::MentionName, "", "x"),
            None
        );
    }
}
