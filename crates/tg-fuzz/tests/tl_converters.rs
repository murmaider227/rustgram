//! Structured fuzz of the converters that take typed server objects (TGB-3086).
//!
//! Every target writes random well-formed TL bytes for one schema type ([`TlGen`]), decodes them
//! with tg-tl's normal `Deserializable` and runs the converter on a guarded worker that records
//! panics, hangs and huge allocations ([`Guard`]). Targets cover tg-api `convert` (DATA), tg-iv
//! rich pages, tg-export `tl_parse`, tg-stories, tg-wallet, tg-passport, tg-bots and tg-payments.
//!
//! `TGFORK_FUZZ_ITERATIONS` raises the per-target count (default [`ITERATIONS`]),
//! `TGFORK_FUZZ_SEED` explores other sequences,
//! `TGFORK_FUZZ_TARGET` runs only the targets whose name contains it, and `--nocapture` prints a
//! summary line per target. Each target's seed is derived from its name, so a finding
//! reproduces with the same name and iteration count.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::OnceLock;

use tg_fuzz::guard::{CountingAlloc, Guard};
use tg_fuzz::tl::{TlGen, TlGenConfig};
use tg_tl::Deserializable;
use tg_tl::{enums as e, types as t};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const ITERATIONS: usize = 150;

/// Findings filed in Linear and not fixed yet: panic location -> issue. A finding at one of
/// these locations is printed, not failed, so the suite stays green until the owning package
/// fixes it; the fix removes the entry (and un-ignores the repro in `tl_known_findings.rs`).
const KNOWN: &[(&str, &str)] = &[];

fn known_issue(key: &str) -> Option<&'static str> {
    let at = key.strip_prefix("panic at ")?.replace('\\', "/");
    KNOWN
        .iter()
        .find(|(location, _)| at.ends_with(location))
        .map(|&(_, issue)| issue)
}
const NOW: i32 = 1_700_000_000;

const SCHEMA: &str = include_str!("../../../schema/api.tl");

fn generator() -> &'static TlGen {
    static GEN: OnceLock<TlGen> = OnceLock::new();
    GEN.get_or_init(|| TlGen::new(SCHEMA).unwrap_or_else(|e| panic!("{e}")))
}

/// Shallow values with short vectors and strings: every other iteration uses them, so the
/// shortest input kept for a finding is usually small enough to read.
fn small_generator() -> &'static TlGen {
    static GEN: OnceLock<TlGen> = OnceLock::new();
    GEN.get_or_init(|| {
        let config = TlGenConfig {
            max_depth: 3,
            max_vector: 2,
            long_vector_one_in: 200,
            long_vector: 8,
            max_nodes: 40,
            max_string_tokens: 3,
        };
        TlGen::with_config(SCHEMA, config).unwrap_or_else(|e| panic!("{e}"))
    })
}

/// Seed of a target: a hash of its name, mixed with `TGFORK_FUZZ_SEED` when set (a deep run
/// with another value explores new sequences; the default reproduces the suite's).
fn seed_of(name: &str) -> u64 {
    let base = std::env::var("TGFORK_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    name.bytes().fold(0xcbf2_9ce4_8422_2325 ^ base, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Findings of one test, across its targets.
#[derive(Default)]
struct Report(Vec<String>);

impl Report {
    /// Fuzzes `case` with values of TL type `tl` decoded as `T`.
    fn target<T, F>(&mut self, name: &str, tl: &str, case: F)
    where
        T: Deserializable + Debug + Send + Sync + 'static,
        F: Fn(&T) + Send + Copy + 'static,
    {
        if let Ok(only) = std::env::var("TGFORK_FUZZ_TARGET")
            && !name.contains(only.as_str())
        {
            return;
        }
        let mut guard = Guard::new(seed_of(name), ITERATIONS);
        let mut decode_errors = 0usize;
        let mut first_error = None;
        for i in 0..guard.iterations() {
            let tl_gen = if i % 2 == 1 {
                small_generator()
            } else {
                generator()
            };
            let Some(bytes) = tl_gen.boxed(guard.rng(), tl) else {
                self.0.push(format!("{name}: `{tl}` is not a schema type"));
                return;
            };
            match T::from_bytes(&bytes) {
                Ok(value) => guard.check(name, i, value, move |v| case(v)),
                Err(err) => {
                    decode_errors += 1;
                    first_error.get_or_insert_with(|| format!("{err:?} at iteration {i}"));
                }
            }
        }
        let stats = guard.stats();
        eprintln!(
            "{name}: {} cases, {decode_errors} decode errors, max alloc/case {} KiB total, \
             {} KiB peak, slowest {:?}",
            stats.cases,
            stats.max_total >> 10,
            stats.max_peak >> 10,
            stats.slowest,
        );
        if let Some(err) = first_error {
            // A decode error means the generator wrote bytes tg-tl does not accept: a bug in
            // the generator (or tg-tl), never a converter finding.
            self.0.push(format!(
                "{name}: {decode_errors} generated `{tl}` values did not decode, first: {err}"
            ));
        }
        for (key, what) in guard.findings() {
            match known_issue(key) {
                Some(issue) => eprintln!("{name}: known finding ({issue}), {key}"),
                None => self.0.push(format!("{key}\n    {what}")),
            }
        }
    }

    fn finish(self) {
        assert!(
            Guard::counting_allocations(),
            "CountingAlloc is not installed"
        );
        assert!(self.0.is_empty(), "findings:\n{}", self.0.join("\n"));
    }
}

fn self_id() -> tg_data::UserId {
    tg_data::UserId(1)
}

fn session() -> tg_data::Session {
    tg_data::Session::new(self_id())
}

#[test]
fn guard_stops_a_huge_allocation_before_it_is_made() {
    let mut guard = Guard::new(1, 1);
    guard.check("huge", 0, 1usize << 40, |&n| {
        let v: Vec<u8> = Vec::with_capacity(n);
        std::hint::black_box(v);
    });
    guard.check("small", 1, 64usize, |&n| {
        let v: Vec<u8> = Vec::with_capacity(n);
        std::hint::black_box(v);
    });
    let keys: Vec<String> = guard.findings().map(|(k, _)| k.to_string()).collect();
    assert_eq!(keys.len(), 1, "{keys:?}");
    assert!(keys[0].starts_with("huge: huge allocation"), "{keys:?}");
}

#[test]
fn generator_reaches_every_constructor_of_a_big_enum() {
    let tl_gen = generator();
    let mut rng = tg_fuzz::Rng::new(seed_of("coverage"));
    let mut seen = std::collections::HashSet::new();
    for _ in 0..4000 {
        let bytes = tl_gen
            .boxed(&mut rng, "PageBlock")
            .unwrap_or_else(|| panic!("PageBlock is a schema type"));
        if let Ok(block) = e::PageBlock::from_bytes(&bytes) {
            seen.insert(std::mem::discriminant(&block));
        }
    }
    assert_eq!(seen.len(), tl_gen.constructor_count("PageBlock"));
}

#[test]
fn tg_api_convert() {
    use tg_api::convert as c;
    let mut r = Report::default();
    r.target("api.message", "Message", |m: &e::Message| {
        let _ = c::message_from_tl(m, self_id());
        let mut s = session();
        let _ = c::apply_tl_message(&mut s, m);
        let _ = c::apply_tl_existing_message(&mut s, m);
    });
    r.target(
        "api.message_media",
        "MessageMedia",
        |m: &e::MessageMedia| {
            let _ = c::message_media_from_tl(m);
        },
    );
    r.target(
        "api.message_action",
        "MessageAction",
        |a: &e::MessageAction| {
            let _ = c::message_action_from_tl(a);
        },
    );
    r.target("api.webpage", "WebPage", |w: &e::WebPage| {
        let _ = c::webpage_from_tl(w);
    });
    r.target("api.document", "Document", |d: &e::Document| {
        let _ = c::document_from_tl(d);
    });
    r.target("api.photo", "Photo", |p: &e::Photo| {
        let _ = c::photo_from_tl(p);
    });
    r.target(
        "api.reactions",
        "MessageReactions",
        |x: &e::MessageReactions| {
            let _ = c::reactions_from_tl(x);
        },
    );
    r.target("api.reply_markup", "ReplyMarkup", |x: &e::ReplyMarkup| {
        let _ = c::reply_markup_from_tl(x);
    });
    r.target(
        "api.entities",
        "Vector<MessageEntity>",
        |x: &Vec<e::MessageEntity>| {
            let _ = c::entities_from_tl(x);
        },
    );
    r.target(
        "api.text_with_entities",
        "TextWithEntities",
        |x: &e::TextWithEntities| {
            let _ = c::text_with_entities_from_tl(x);
        },
    );
    r.target(
        "api.fwd_header",
        "MessageFwdHeader",
        |x: &e::MessageFwdHeader| {
            let _ = c::forward_info_from_tl(x);
        },
    );
    r.target(
        "api.reply_header",
        "MessageReplyHeader",
        |x: &e::MessageReplyHeader| {
            let _ = c::reply_info_from_tl(x);
        },
    );
    r.target(
        "api.dialog_filter",
        "DialogFilter",
        |x: &e::DialogFilter| {
            let _ = c::dialog_filter_from_tl(x, Some(self_id()));
        },
    );
    r.target("api.draft", "DraftMessage", |x: &e::DraftMessage| {
        let _ = c::draft_from_tl(x);
    });
    r.target(
        "api.typing",
        "SendMessageAction",
        |x: &e::SendMessageAction| {
            let _ = c::typing_action_from_tl(x);
        },
    );
    r.target(
        "api.notify",
        "PeerNotifySettings",
        |x: &e::PeerNotifySettings| {
            let _ = c::peer_notify_settings_from_tl(x);
        },
    );
    r.target("api.emoji_status", "EmojiStatus", |x: &e::EmojiStatus| {
        let _ = c::emoji_status_from_tl(Some(x));
    });
    r.target("api.users", "Vector<User>", |x: &Vec<e::User>| {
        c::apply_tl_users_and_chats(&mut session(), x, &[]);
    });
    r.target("api.chats", "Vector<Chat>", |x: &Vec<e::Chat>| {
        c::apply_tl_users_and_chats(&mut session(), &[], x);
    });
    r.target("api.user_full", "UserFull", |x: &e::UserFull| {
        let _ = c::apply_tl_user_full(&mut session(), x);
    });
    r.target("api.chat_full", "ChatFull", |x: &e::ChatFull| {
        let _ = c::apply_tl_chat_full(&mut session(), x);
    });
    r.target(
        "api.dialogs",
        "messages.Dialogs",
        |x: &e::messages::Dialogs| {
            c::apply_tl_dialogs_response(&mut session(), x, NOW);
        },
    );
    r.target(
        "api.peer_dialogs",
        "messages.peerDialogs",
        |x: &t::messages::PeerDialogs| {
            c::apply_tl_peer_dialogs(&mut session(), x, NOW);
        },
    );
    r.target(
        "api.short_message",
        "updateShortMessage",
        |x: &t::UpdateShortMessage| {
            let _ = c::message_from_short_message(x, self_id());
        },
    );
    r.target(
        "api.short_chat_message",
        "updateShortChatMessage",
        |x: &t::UpdateShortChatMessage| {
            let _ = c::message_from_short_chat_message(x, self_id());
        },
    );
    r.target(
        "api.secure_secret_algo",
        "SecurePasswordKdfAlgo",
        |x: &e::SecurePasswordKdfAlgo| {
            let _ = tg_api::account::parse_secure_secret_algo(x);
        },
    );
    r.finish();
}

#[test]
fn tg_iv_rich_pages() {
    use tg_iv::rich_page::{
        RichParseMode, rich_page_from_tl_blocks, rich_page_from_tl_page,
        rich_page_from_tl_rich_message,
    };
    let mut r = Report::default();
    r.target("iv.blocks", "Vector<PageBlock>", |x: &Vec<e::PageBlock>| {
        let _ = rich_page_from_tl_blocks(x, RichParseMode::Normal);
        let _ = rich_page_from_tl_blocks(x, RichParseMode::DisplayTextDiff);
    });
    r.target("iv.rich_message", "richMessage", |x: &t::RichMessage| {
        let _ = rich_page_from_tl_rich_message(x, RichParseMode::Normal);
        let _ = rich_page_from_tl_rich_message(x, RichParseMode::DisplayTextDiff);
    });
    r.target("iv.page", "page", |x: &t::Page| {
        let _ = rich_page_from_tl_page(x);
    });
    r.finish();
}

fn dialogs_parts(
    x: &e::messages::Dialogs,
) -> (&[e::Dialog], &[e::Message], &[e::User], &[e::Chat]) {
    match x {
        e::messages::Dialogs::Dialogs(d) => (&d.dialogs, &d.messages, &d.users, &d.chats),
        e::messages::Dialogs::Slice(d) => (&d.dialogs, &d.messages, &d.users, &d.chats),
        e::messages::Dialogs::NotModified(_) => (&[], &[], &[], &[]),
    }
}

fn messages_parts(x: &e::messages::Messages) -> (&[e::Message], &[e::User], &[e::Chat]) {
    match x {
        e::messages::Messages::Messages(m) => (&m.messages, &m.users, &m.chats),
        e::messages::Messages::Slice(m) => (&m.messages, &m.users, &m.chats),
        e::messages::Messages::ChannelMessages(m) => (&m.messages, &m.users, &m.chats),
        e::messages::Messages::NotModified(_) => (&[], &[], &[]),
    }
}

#[test]
fn tg_export_tl_parse() {
    use tg_export::tl_parse as p;
    let self_peer = tg_data::PeerId::from_user(self_id());
    let mut r = Report::default();
    r.target("export.message", "Message", move |x: &e::Message| {
        let _ = p::parse_message(x, self_peer, "chats/chat_01/");
    });
    r.target("export.media", "MessageMedia", |x: &e::MessageMedia| {
        let _ = p::parse_media(x, "chats/chat_01/", NOW);
    });
    r.target(
        "export.action",
        "MessageAction",
        move |x: &e::MessageAction| {
            let _ = p::parse_service_action(x, self_peer);
        },
    );
    r.target("export.rich_text", "RichText", |x: &e::RichText| {
        let _ = p::parse_rich_text(x);
    });
    r.target(
        "export.rich_message",
        "RichMessage",
        |x: &e::RichMessage| {
            let _ = p::parse_rich_message(x);
        },
    );
    r.target(
        "export.reply_markup",
        "ReplyMarkup",
        |x: &e::ReplyMarkup| {
            let _ = p::parse_button_rows(x);
        },
    );
    r.target(
        "export.reactions",
        "MessageReactions",
        |x: &e::MessageReactions| {
            let _ = p::parse_reactions(x);
        },
    );
    r.target(
        "export.messages",
        "messages.Messages",
        move |x: &e::messages::Messages| {
            let (messages, users, chats) = messages_parts(x);
            let _ = p::parse_messages_slice(users, chats, messages, self_peer, "chats/chat_01/");
        },
    );
    r.target(
        "export.dialogs",
        "messages.Dialogs",
        |x: &e::messages::Dialogs| {
            let (dialogs, messages, users, chats) = dialogs_parts(x);
            let _ = p::parse_dialogs_info(dialogs, messages, users, chats, 0);
            let _ = p::parse_peers_lists(users, chats);
            let _ = p::parse_left_channels_info(chats);
        },
    );
    r.target(
        "export.stories",
        "Vector<StoryItem>",
        |x: &Vec<e::StoryItem>| {
            let _ = p::parse_stories_slice(x, 0, 3 * 3600);
        },
    );
    r.target("export.userpics", "Vector<Photo>", |x: &Vec<e::Photo>| {
        let _ = p::parse_userpics_slice(x, 0, -5 * 3600);
    });
    r.target(
        "export.personal_info",
        "users.UserFull",
        |x: &e::users::UserFull| {
            let _ = p::parse_personal_info(x);
        },
    );
    r.target(
        "export.contacts",
        "Vector<SavedContact>",
        |x: &Vec<e::SavedContact>| {
            let _ = p::parse_contacts_list(x);
        },
    );
    r.target(
        "export.top_peers",
        "contacts.TopPeers",
        |x: &e::contacts::TopPeers| {
            let mut list = p::parse_contacts_list(&[]);
            let _ = p::append_top_peers(&mut list, x);
        },
    );
    r.target(
        "export.sessions",
        "account.Authorizations",
        |x: &e::account::Authorizations| {
            let _ = p::parse_sessions_list(x);
        },
    );
    r.target(
        "export.web_sessions",
        "account.WebAuthorizations",
        |x: &e::account::WebAuthorizations| {
            let _ = p::parse_web_sessions_list(x);
        },
    );
    r.finish();
}

#[test]
fn tg_stories_tl() {
    use tg_stories::StoryPeerContext;
    use tg_stories::tl as s;
    let ctx = |_| StoryPeerContext {
        is_user: true,
        can_edit_stories: true,
        ..StoryPeerContext::default()
    };
    let peer = tg_data::PeerId::from_user(self_id());
    let mut r = Report::default();
    r.target("stories.item", "StoryItem", move |x: &e::StoryItem| {
        let _ = s::story_item_from_tl(peer, ctx(peer), x, NOW, None);
    });
    r.target(
        "stories.media_areas",
        "Vector<MediaArea>",
        |x: &Vec<e::MediaArea>| {
            let _ = s::parse_media_areas(x, &BTreeMap::new());
        },
    );
    r.target(
        "stories.privacy",
        "Vector<PrivacyRule>",
        |x: &Vec<e::PrivacyRule>| {
            let _ = s::parse_privacy_rules(x);
            let _ = s::parse_privacy_rules_with(x, |id| Some(tg_data::PeerId(id)));
        },
    );
    r.target("stories.views", "StoryViews", |x: &e::StoryViews| {
        let _ = s::story_views_counts_from_tl(x, None, false);
        let _ = s::story_views_counts_from_tl(x, None, true);
    });
    r.target("stories.peer", "PeerStories", move |x: &e::PeerStories| {
        let _ = s::peer_stories_from_tl(x, NOW, ctx);
    });
    r.target(
        "stories.all",
        "stories.AllStories",
        move |x: &e::stories::AllStories| {
            let _ = s::all_stories_from_tl(x, NOW, ctx);
        },
    );
    r.target(
        "stories.page",
        "stories.Stories",
        move |x: &e::stories::Stories| {
            let _ = s::stories_page_from_tl(peer, ctx(peer), x, NOW);
        },
    );
    r.target(
        "stories.views_list",
        "stories.StoryViewsList",
        move |x: &e::stories::StoryViewsList| {
            let _ = s::story_views_list_from_tl(x, NOW, ctx);
        },
    );
    r.target(
        "stories.reactions_list",
        "stories.StoryReactionsList",
        move |x: &e::stories::StoryReactionsList| {
            let _ = s::story_reactions_list_from_tl(x, NOW, ctx);
        },
    );
    r.target("stories.update", "Update", move |x: &e::Update| {
        let _ = s::story_update_from_tl(x, NOW, ctx);
    });
    r.target(
        "stories.albums",
        "stories.Albums",
        |x: &e::stories::Albums| {
            let _ = s::albums_from_tl(x);
        },
    );
    r.finish();
}

#[test]
fn tg_wallet_mtproto_api() {
    use tg_wallet::mtproto_api as w;
    let mut r = Report::default();
    r.target("wallet.state", "WalletState", |x: &e::WalletState| {
        let _ = w::parse_wallet_state(x.clone());
    });
    r.target(
        "wallet.user_addresses",
        "wallet.UserAddresses",
        |x: &e::wallet::UserAddresses| {
            let _ = w::parse_user_addresses(x.clone());
        },
    );
    r.target(
        "wallet.nft_page",
        "wallet.NftItems",
        |x: &e::wallet::NftItems| {
            let _ = w::parse_nft_page(x.clone(), "");
            let _ = w::parse_nft_page(x.clone(), "offset");
        },
    );
    r.target(
        "wallet.nft_item",
        "wallet.NftItem",
        |x: &e::wallet::NftItem| {
            let _ = w::parse_server_nft_item(x.clone());
        },
    );
    r.target(
        "wallet.transaction",
        "WalletTransaction",
        |x: &e::WalletTransaction| {
            let _ = w::parse_server_transaction(x.clone());
        },
    );
    r.finish();
}

#[test]
fn tg_passport_algos() {
    use tg_passport::controller as pc;
    let mut r = Report::default();
    r.target(
        "passport.kdf",
        "PasswordKdfAlgo",
        |x: &e::PasswordKdfAlgo| {
            let _ = pc::parse_password_kdf_algo(x);
        },
    );
    r.target(
        "passport.secure_kdf",
        "SecurePasswordKdfAlgo",
        |x: &e::SecurePasswordKdfAlgo| {
            let _ = pc::parse_secure_secret_algo(x);
        },
    );
    r.finish();
}

#[test]
fn tg_api_extras() {
    use tg_api::file_origin::UpdatedFileReferences;
    let mut r = Report::default();
    r.target(
        "api.countries",
        "help.CountriesList",
        |x: &e::help::CountriesList| {
            let _ = tg_api::lang::countries_list_from_tl(x);
        },
    );
    r.target(
        "api.file_refs.messages",
        "messages.Messages",
        |x: &e::messages::Messages| {
            UpdatedFileReferences::new().push_messages(x);
        },
    );
    r.target(
        "api.file_refs.chat_full",
        "messages.ChatFull",
        |x: &e::messages::ChatFull| {
            UpdatedFileReferences::new().push_chat_full(x);
        },
    );
    r.target(
        "api.file_refs.promo",
        "help.PremiumPromo",
        |x: &e::help::PremiumPromo| {
            UpdatedFileReferences::new().push_premium_promo(x);
        },
    );
    r.finish();
}

#[test]
fn tg_stories_store_and_wallet_updates() {
    let peer = tg_data::PeerId::from_user(self_id());
    let mut r = Report::default();
    r.target("stories.store_update", "Update", move |x: &e::Update| {
        let _ = tg_stories::StoriesStore::new(peer).apply_tl_update(x, NOW);
    });
    r.target("wallet.update", "Update", |x: &e::Update| {
        let _ = tg_wallet::mtproto_api::parse_wallet_update(x);
    });
    r.target("wallet.updates", "Updates", |x: &e::Updates| {
        let _ = tg_wallet::mtproto_api::extract_sent_transfer_receipt(x);
        let _ = tg_wallet::mtproto_api::extract_gasless_info(x);
    });
    r.finish();
}

#[test]
fn tg_bots_tl_convert() {
    use tg_bots::tl_convert as b;
    let mut r = Report::default();
    r.target("bots.reply_markup", "ReplyMarkup", |x: &e::ReplyMarkup| {
        let _ = b::reply_markup_from_tl(x);
    });
    r.target(
        "bots.callback_answer",
        "messages.BotCallbackAnswer",
        |x: &e::messages::BotCallbackAnswer| {
            let _ = b::callback_answer_from_tl(x);
        },
    );
    r.target("bots.url_auth", "UrlAuthResult", |x: &e::UrlAuthResult| {
        let _ = b::url_auth_result_from_tl(x);
    });
    r.target(
        "bots.results",
        "messages.BotResults",
        |x: &e::messages::BotResults| {
            let _ = b::bot_results_from_tl("query", x);
        },
    );
    r.target(
        "bots.menu_button",
        "BotMenuButton",
        |x: &e::BotMenuButton| {
            let _ = b::bot_menu_button_from_tl(x);
        },
    );
    r.target(
        "bots.app_settings",
        "BotAppSettings",
        |x: &e::BotAppSettings| {
            let _ = b::bot_app_colors_from_tl(x);
        },
    );
    r.target(
        "bots.verifier",
        "BotVerifierSettings",
        |x: &e::BotVerifierSettings| {
            let _ = b::bot_verifier_settings_from_tl(x);
        },
    );
    r.target("bots.info", "BotInfo", |x: &e::BotInfo| {
        let _ = b::bot_info_update_from_tl(x);
    });
    r.target(
        "bots.attach_menu",
        "AttachMenuBots",
        |x: &e::AttachMenuBots| {
            let _ = b::attach_menu_bots_from_tl(x);
        },
    );
    r.target("bots.web_view", "WebViewResult", |x: &e::WebViewResult| {
        let _ = b::web_view_result_from_tl(x);
    });
    r.target(
        "bots.high_scores",
        "messages.HighScores",
        |x: &e::messages::HighScores| {
            let _ = b::game_high_scores_from_tl(x);
        },
    );
    r.target("bots.app", "messages.BotApp", |x: &e::messages::BotApp| {
        let _ = b::bot_app_from_tl(tg_bots::stand_in::UserId(1), x);
    });
    r.target(
        "bots.popular",
        "bots.PopularAppBots",
        |x: &e::bots::PopularAppBots| {
            let _ = b::popular_app_bots_from_tl(x);
        },
    );
    r.target(
        "bots.recommendations",
        "users.Users",
        |x: &e::users::Users| {
            let _ = b::bot_recommendations_from_tl(x);
        },
    );
    r.target(
        "bots.webview_button",
        "KeyboardButton",
        |x: &e::KeyboardButton| {
            let _ = b::requested_webview_button_from_tl(x);
        },
    );
    r.finish();
}

#[test]
fn tg_payments_from_tl() {
    use tg_payments::{credits, premium, star_gift_auctions, star_gifts, starref};
    let peer = tg_data::PeerId::from_user(self_id());
    let mut r = Report::default();
    r.target(
        "payments.stars_status",
        "payments.StarsStatus",
        move |x: &e::payments::StarsStatus| {
            let _ = credits::stars_status_from_tl(x, peer, NOW);
        },
    );
    r.target(
        "payments.subscription",
        "StarsSubscription",
        |x: &e::StarsSubscription| {
            let _ = credits::stars_subscription_from_tl(x, NOW);
        },
    );
    r.target(
        "payments.topup",
        "Vector<StarsTopupOption>",
        |x: &Vec<e::StarsTopupOption>| {
            let _ = credits::stars_topup_options_from_tl(x);
        },
    );
    r.target(
        "payments.giveaway_options",
        "Vector<StarsGiveawayOption>",
        |x: &Vec<e::StarsGiveawayOption>| {
            let _ = credits::stars_giveaway_options_from_tl(x);
        },
    );
    r.target(
        "payments.revenue",
        "payments.StarsRevenueStats",
        |x: &e::payments::StarsRevenueStats| {
            let _ = credits::stars_revenue_stats_from_tl(x, String::new());
        },
    );
    r.target(
        "payments.premium_options",
        "Vector<PremiumSubscriptionOption>",
        |x: &Vec<e::PremiumSubscriptionOption>| {
            let _ = premium::premium_subscription_options_from_tl(x);
        },
    );
    r.target(
        "payments.gift_code_options",
        "Vector<PremiumGiftCodeOption>",
        |x: &Vec<e::PremiumGiftCodeOption>| {
            let _ = premium::gift_code_options_from_tl(x);
        },
    );
    r.target(
        "payments.premium_promo",
        "help.PremiumPromo",
        |x: &e::help::PremiumPromo| {
            let _ = premium::premium_promo_from_tl(x);
        },
    );
    r.target(
        "payments.gift_code",
        "payments.CheckedGiftCode",
        |x: &e::payments::CheckedGiftCode| {
            let _ = premium::checked_gift_code_from_tl(x);
        },
    );
    r.target(
        "payments.giveaway_info",
        "payments.GiveawayInfo",
        |x: &e::payments::GiveawayInfo| {
            let _ = premium::giveaway_info_from_tl(x);
        },
    );
    r.target(
        "payments.boosts_status",
        "premium.BoostsStatus",
        |x: &e::premium::BoostsStatus| {
            let _ = premium::boosts_status_from_tl(x, false);
            let _ = premium::boosts_status_from_tl(x, true);
        },
    );
    r.target(
        "payments.boosts_list",
        "premium.BoostsList",
        move |x: &e::premium::BoostsList| {
            let _ = premium::boosts_list_from_tl(x, peer, false);
        },
    );
    r.target(
        "payments.my_boosts",
        "premium.MyBoosts",
        |x: &e::premium::MyBoosts| {
            let _ = premium::my_boosts_from_tl(x);
        },
    );
    r.target(
        "payments.auction",
        "payments.StarGiftAuctionState",
        |x: &e::payments::StarGiftAuctionState| {
            let _ = star_gift_auctions::auction_state_from_tl(x);
        },
    );
    r.target(
        "payments.active_auctions",
        "payments.StarGiftActiveAuctions",
        |x: &e::payments::StarGiftActiveAuctions| {
            let _ = star_gift_auctions::active_auctions_from_tl(x);
        },
    );
    r.target("payments.star_gift", "StarGift", |x: &e::StarGift| {
        let _ = star_gifts::star_gift_from_tl(x);
    });
    r.target(
        "payments.saved_gifts",
        "payments.SavedStarGifts",
        move |x: &e::payments::SavedStarGifts| {
            let _ = star_gifts::saved_star_gifts_from_tl(x, peer, true);
        },
    );
    r.target(
        "payments.resale",
        "payments.ResaleStarGifts",
        |x: &e::payments::ResaleStarGifts| {
            let _ = star_gifts::resale_star_gifts_from_tl(7, x);
        },
    );
    r.target(
        "payments.unique_value",
        "payments.UniqueStarGiftValueInfo",
        |x: &e::payments::UniqueStarGiftValueInfo| {
            let _ = star_gifts::unique_gift_value_from_tl(x);
        },
    );
    r.target(
        "payments.upgrade_preview",
        "payments.StarGiftUpgradePreview",
        |x: &e::payments::StarGiftUpgradePreview| {
            let _ = star_gifts::star_gift_upgrade_preview_from_tl(x);
        },
    );
    r.target(
        "payments.upgrade_attributes",
        "payments.StarGiftUpgradeAttributes",
        |x: &e::payments::StarGiftUpgradeAttributes| {
            let _ = star_gifts::star_gift_upgrade_attributes_from_tl(x);
        },
    );
    r.target(
        "payments.starref",
        "payments.ConnectedStarRefBots",
        |x: &e::payments::ConnectedStarRefBots| {
            let _ = starref::connected_star_ref_bots_from_tl(x);
        },
    );
    r.target(
        "payments.starref_suggested",
        "payments.SuggestedStarRefBots",
        |x: &e::payments::SuggestedStarRefBots| {
            let _ = starref::suggested_star_ref_bots_from_tl(x);
        },
    );
    r.finish();
}
