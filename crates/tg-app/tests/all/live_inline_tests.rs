//! Cross-crate unit tests moved from `live/{stats,stories,suggestions}.rs`.

mod stats_tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tg_api::convert::apply_tl_users_and_chats;
    use tg_api::{DcId, InvocationError, RpcClient};
    use tg_data::{ChannelId, PeerId as DataPeerId, UserId as DataUserId};
    use tg_session::{SessionRuntime, SessionRuntimeConfig};
    use tg_stats::{
        BoostsOffsetToken, ChartPalette, CreditsType, DeltaTrend, StatisticalCurrency,
        channel_overview_blocks,
    };
    use tg_tl::{
        Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums,
        functions as tl_fns, types as tl_types,
    };
    use tokio::sync::broadcast;

    use tg_app::live::LiveSession;
    use tg_app::live::stats::{statistical_graph_from_tl, statistical_value_from_tl};
    use tg_app::stand_in::types::PeerId;
    use tg_app::statistics::backend::StatsBackend;
    use tg_app::statistics::sections::ChannelStatsView;

    const TEST_TIMEOUT: Duration = Duration::from_secs(10);

    const GROWTH_JSON: &str = r#"{
        "columns": [
            ["x", 1700006400000, 1700092800000, 1700179200000, 1700265600000],
            ["y0", 1200, 1250, 1310, 1400]
        ],
        "types": { "x": "x", "y0": "line" },
        "names": { "y0": "Followers" },
        "colors": { "y0": "BLUE#327fe5" },
        "subchart": { "show": true }
    }"#;

    const ZOOM_HOURLY_JSON: &str = r#"{
        "columns": [
            ["x", 1700006400000, 1700010000000, 1700013600000],
            ["y0", 15, 22, 18]
        ],
        "types": { "x": "x", "y0": "line" },
        "names": { "y0": "Hourly" },
        "colors": { "y0": "GREEN#61c752" },
        "subchart": { "show": false }
    }"#;

    const TON_REVENUE_JSON: &str = r#"{
        "columns": [
            ["x", 1700006400000, 1700092800000],
            ["y0", 1500000000, 2250000000]
        ],
        "types": { "x": "x", "y0": "bar" },
        "names": { "y0": "Ad Revenue" },
        "colors": { "y0": "BLUE#327fe5" },
        "stacked": true,
        "yTickFormatter": "statsFormat('TON')"
    }"#;

    const STARS_REVENUE_JSON: &str = r#"{
        "columns": [
            ["x", 1700006400000, 1700092800000],
            ["y0", 300, 450]
        ],
        "types": { "x": "x", "y0": "bar" },
        "names": { "y0": "Stars" },
        "colors": { "y0": "GOLDEN#eba52d" },
        "stacked": true,
        "yTickFormatter": "statsFormat('XTR')"
    }"#;

    type RecordedCall = (u32, Vec<u8>);
    type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);

    #[derive(Clone)]
    struct FakeStatsRpc {
        recorded: Arc<Mutex<Vec<RecordedCall>>>,
        responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
    }

    impl FakeStatsRpc {
        fn new() -> Self {
            Self {
                recorded: Arc::new(Mutex::new(Vec::new())),
                responses: Arc::new(Mutex::new(VecDeque::new())),
            }
        }

        fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
            self.responses
                .lock()
                .expect("lock responses")
                .push_back((expected_ctor, Ok(resp.to_bytes())));
        }

        fn recorded_ctors(&self) -> Vec<u32> {
            self.recorded
                .lock()
                .expect("lock recorded")
                .iter()
                .map(|(c, _)| *c)
                .collect()
        }

        fn recorded_calls_of(&self, ctor: u32) -> Vec<Vec<u8>> {
            self.recorded
                .lock()
                .expect("lock recorded")
                .iter()
                .filter(|(c, _)| *c == ctor)
                .map(|(_, b)| b.clone())
                .collect()
        }
    }

    impl RpcClient for FakeStatsRpc {
        type Error = InvocationError;

        async fn invoke<R: RemoteCall + Send + Sync>(
            &self,
            req: &R,
        ) -> Result<R::Return, Self::Error> {
            self.invoke_in_dc(2, req).await
        }

        async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
            &self,
            _dc_id: DcId,
            req: &R,
        ) -> Result<R::Return, Self::Error> {
            let bytes = req.to_bytes();
            let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            self.recorded
                .lock()
                .expect("lock recorded")
                .push((ctor, bytes));

            let next = {
                let mut q = self.responses.lock().expect("lock responses");
                q.iter()
                    .position(|(c, _)| *c == ctor)
                    .and_then(|pos| q.remove(pos).map(|(_, r)| r))
            };
            let raw = next.unwrap_or(Err(InvocationError::Dropped))?;
            R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
        }

        fn main_dc_id(&self) -> DcId {
            2
        }

        fn set_main_dc_id(&self, _dc_id: DcId) {}

        fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
            None
        }
    }

    fn tl_graph_data(json: &str, zoom_token: Option<&str>) -> tl_enums::StatsGraph {
        tl_enums::StatsGraph::StatsGraph(tl_types::StatsGraph {
            json: tl_enums::DataJSON::DataJSON(tl_types::DataJSON {
                data: json.to_owned(),
            }),
            zoom_token: zoom_token.map(ToOwned::to_owned),
        })
    }

    fn tl_graph_async(token: &str) -> tl_enums::StatsGraph {
        tl_enums::StatsGraph::Async(tl_types::StatsGraphAsync {
            token: token.to_owned(),
        })
    }

    fn tl_graph_error(err: &str) -> tl_enums::StatsGraph {
        tl_enums::StatsGraph::Error(tl_types::StatsGraphError {
            error: err.to_owned(),
        })
    }

    fn tl_abs_prev(current: f64, previous: f64) -> tl_enums::StatsAbsValueAndPrev {
        tl_enums::StatsAbsValueAndPrev::StatsAbsValueAndPrev(tl_types::StatsAbsValueAndPrev {
            current,
            previous,
        })
    }

    fn tl_user(id: i64, name: &str, username: &str, bot: bool) -> tl_enums::User {
        tl_enums::User::User(tl_types::User {
            self_: false,
            contact: false,
            mutual_contact: false,
            deleted: false,
            bot,
            bot_chat_history: false,
            bot_nochats: false,
            verified: false,
            restricted: false,
            min: false,
            bot_inline_geo: false,
            support: false,
            scam: false,
            apply_min_photo: false,
            fake: false,
            bot_attach_menu: false,
            premium: false,
            attach_menu_enabled: false,
            bot_can_edit: false,
            close_friend: false,
            stories_hidden: false,
            stories_unavailable: false,
            contact_require_premium: false,
            bot_business: false,
            bot_has_main_app: false,
            bot_forum_view: false,
            bot_forum_can_manage_topics: false,
            bot_can_manage_bots: false,
            bot_guestchat: false,
            bot_guard: false,
            bot_forum_sidebar: false,
            id,
            access_hash: Some(id * 11 + 3),
            first_name: Some(name.to_owned()),
            last_name: None,
            username: Some(username.to_owned()),
            phone: None,
            photo: None,
            status: None,
            bot_info_version: None,
            restriction_reason: None,
            bot_inline_placeholder: None,
            lang_code: None,
            emoji_status: None,
            usernames: None,
            stories_max_id: None,
            color: None,
            profile_color: None,
            bot_active_users: None,
            bot_verification_icon: None,
            send_paid_messages_stars: None,
            linked_community_id: None,
        })
    }

    fn tl_channel(id: i64, title: &str, username: &str, megagroup: bool) -> tl_enums::Chat {
        tl_enums::Chat::Channel(tl_types::Channel {
            creator: true,
            left: false,
            broadcast: !megagroup,
            verified: false,
            megagroup,
            restricted: false,
            signatures: false,
            min: false,
            scam: false,
            has_link: false,
            has_geo: false,
            slowmode_enabled: false,
            call_active: false,
            call_not_empty: false,
            fake: false,
            gigagroup: false,
            noforwards: false,
            join_to_send: false,
            join_request: false,
            forum: false,
            stories_hidden: false,
            stories_hidden_min: false,
            stories_unavailable: false,
            signature_profiles: false,
            autotranslation: false,
            broadcast_messages_allowed: false,
            monoforum: false,
            forum_tabs: false,
            in_news: false,
            forum_sidebar: false,
            id,
            access_hash: Some(id * 101 + 5),
            title: title.to_owned(),
            username: Some(username.to_owned()),
            photo: tl_enums::ChatPhoto::Empty,
            date: 1_700_000_000,
            restriction_reason: None,
            admin_rights: None,
            banned_rights: None,
            default_banned_rights: None,
            participants_count: Some(5000),
            usernames: None,
            stories_max_id: None,
            color: None,
            profile_color: None,
            emoji_status: None,
            level: Some(4),
            subscription_until_date: None,
            bot_verification_icon: None,
            send_paid_messages_stars: None,
            linked_monoforum_id: None,
            linked_community_id: None,
            pinned_msg_id: None,
        })
    }

    struct StatsHarness {
        fake: FakeStatsRpc,
        runtime: Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
        live: LiveSession,
        channel_peer: PeerId,
        supergroup_peer: PeerId,
        bot_peer: PeerId,
    }

    impl StatsHarness {
        fn start() -> Self {
            let self_uid = 810_001i64;
            let channel_id = 910_001i64;
            let supergroup_id = 910_002i64;
            let bot_uid = 810_099i64;

            let fake = FakeStatsRpc::new();
            let cfg = SessionRuntimeConfig {
                init_updates_state: false,
                run_get_difference_on_start: false,
                load_initial_dialogs: false,
                fetch_self_user_on_start: false,
                ..SessionRuntimeConfig::default()
            };
            let runtime = Arc::new(
                SessionRuntime::start(
                    DataUserId::from_tl(self_uid),
                    tg_app::live::ErasedRpcClient::new(fake.clone()),
                    cfg,
                )
                .expect("start runtime"),
            );
            runtime
                .tokio_handle()
                .block_on(async { tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready()).await })
                .expect("wait_ready timeout")
                .expect("ready");

            runtime.tokio_handle().block_on(async {
                let mut s = runtime.session().write().await;
                apply_tl_users_and_chats(
                    &mut s,
                    &[
                        tl_user(self_uid, "Owner", "owner_u", false),
                        tl_user(bot_uid, "StatsBot", "stats_bot", true),
                    ],
                    &[
                        tl_channel(channel_id, "Tech News", "tech_news", false),
                        tl_channel(supergroup_id, "Rust Forum", "rust_forum", true),
                    ],
                );
            });

            let live = LiveSession::new(Arc::clone(&runtime), 1);
            let channel_peer = PeerId(DataPeerId::from_channel(ChannelId::from_tl(channel_id)).0);
            let supergroup_peer =
                PeerId(DataPeerId::from_channel(ChannelId::from_tl(supergroup_id)).0);
            let bot_peer = PeerId(DataPeerId::from_user(DataUserId::from_tl(bot_uid)).0);

            Self {
                fake,
                runtime,
                live,
                channel_peer,
                supergroup_peer,
                bot_peer,
            }
        }

        fn shutdown(self) {
            drop(self.live);
            if let Ok(rt) = Arc::try_unwrap(self.runtime) {
                rt.shutdown();
            }
        }
    }

    #[test]
    fn statistical_graph_handles_data_async_and_error() {
        let ok_graph = statistical_graph_from_tl(&tl_graph_data(GROWTH_JSON, Some("tok_zoom")));
        assert_eq!(ok_graph.chart.x.len(), 4);
        assert_eq!(ok_graph.chart.lines.len(), 1);
        assert_eq!(ok_graph.zoom_token, "tok_zoom");
        assert!(ok_graph.error.is_empty());

        let async_graph = statistical_graph_from_tl(&tl_graph_async("tok_async_123"));
        assert!(async_graph.chart.is_empty());
        assert_eq!(async_graph.zoom_token, "tok_async_123");
        assert!(async_graph.error.is_empty());

        let err_graph = statistical_graph_from_tl(&tl_graph_error("GRAPH_NOT_READY"));
        assert!(err_graph.chart.is_empty());
        assert!(err_graph.zoom_token.is_empty());
        assert_eq!(err_graph.error, "GRAPH_NOT_READY");

        let val = statistical_value_from_tl(&tl_abs_prev(1500.0, 1200.0));
        assert_eq!(val.value, 1500.0);
        assert_eq!(val.previous_value, 1200.0);
        assert!((val.growth_rate_percentage - 25.0).abs() < 1e-9);
    }

    #[test]
    fn broadcast_stats_renders_overview_cards_and_parsed_chart_and_async_zoom() {
        let h = StatsHarness::start();
        h.live.set_stats_target_peer(h.channel_peer);

        // BroadcastStats includes one regular chart, one StatsGraphAsync (hydrated via loadAsyncGraph),
        // and one StatsGraphError.
        h.fake.enqueue_ok(
            tl_fns::stats::GetBroadcastStats::CONSTRUCTOR_ID,
            &tl_enums::stats::BroadcastStats::BroadcastStats(tl_types::stats::BroadcastStats {
                period: tl_enums::StatsDateRangeDays::StatsDateRangeDays(
                    tl_types::StatsDateRangeDays {
                        min_date: 1_700_006_400,
                        max_date: 1_701_129_600,
                    },
                ),
                followers: tl_abs_prev(14_200.0, 13_000.0),
                views_per_post: tl_abs_prev(9_500.0, 10_000.0),
                shares_per_post: tl_abs_prev(220.0, 200.0),
                reactions_per_post: tl_abs_prev(310.0, 280.0),
                views_per_story: tl_abs_prev(4_100.0, 3_900.0),
                shares_per_story: tl_abs_prev(45.0, 40.0),
                reactions_per_story: tl_abs_prev(85.0, 75.0),
                enabled_notifications: tl_enums::StatsPercentValue::StatsPercentValue(
                    tl_types::StatsPercentValue {
                        part: 8_520.0,
                        total: 14_200.0,
                    },
                ),
                growth_graph: tl_graph_data(GROWTH_JSON, Some("zoom_growth_tok")),
                followers_graph: tl_graph_async("async_followers_tok"),
                mute_graph: tl_graph_error("TEMPORARILY_UNAVAILABLE"),
                top_hours_graph: tl_graph_error("EMPTY"),
                interactions_graph: tl_graph_error("EMPTY"),
                iv_interactions_graph: tl_graph_error("EMPTY"),
                views_by_source_graph: tl_graph_error("EMPTY"),
                new_followers_by_source_graph: tl_graph_error("EMPTY"),
                languages_graph: tl_graph_error("EMPTY"),
                reactions_by_emotion_graph: tl_graph_error("EMPTY"),
                story_interactions_graph: tl_graph_error("EMPTY"),
                story_reactions_by_emotion_graph: tl_graph_error("EMPTY"),
                recent_posts_interactions: vec![
                    tl_enums::PostInteractionCounters::Message(
                        tl_types::PostInteractionCountersMessage {
                            msg_id: 77,
                            views: 8_400,
                            forwards: 120,
                            reactions: 340,
                        },
                    ),
                    tl_enums::PostInteractionCounters::Story(
                        tl_types::PostInteractionCountersStory {
                            story_id: 12,
                            views: 3_100,
                            forwards: 19,
                            reactions: 75,
                        },
                    ),
                ],
            }),
        );
        // Enqueue hydration response for `async_followers_tok`.
        h.fake.enqueue_ok(
            tl_fns::stats::LoadAsyncGraph::CONSTRUCTOR_ID,
            &tl_graph_data(GROWTH_JSON, None),
        );

        let bundle = h.live.channel_statistics().expect("channel_statistics");
        assert_eq!(bundle.peer.title, "Tech News");
        assert_eq!(bundle.stats.member_count.value, 14_200.0);
        assert!((bundle.stats.enabled_notifications_percentage - 60.0).abs() < 1e-6);
        assert_eq!(bundle.stats.member_count_graph.chart.x.len(), 4);
        assert_eq!(bundle.stats.join_graph.chart.x.len(), 4);
        assert_eq!(bundle.stats.mute_graph.error, "TEMPORARILY_UNAVAILABLE");
        assert_eq!(bundle.recent_posts.len(), 2);
        assert_eq!(bundle.recent_posts[0].interaction.message_id, 77);
        assert!(bundle.recent_posts[1].is_story);

        // Verify overview cards + ChannelStatsView construction.
        let palette = ChartPalette::light();
        let (primary, secondary) = channel_overview_blocks(&bundle.stats, &palette);
        assert_eq!(primary.cards.len(), 4);
        assert_eq!(primary.cards[0].caption, "Followers");
        assert_eq!(
            primary.cards[0].delta.as_ref().map(|d| d.trend),
            Some(DeltaTrend::Positive)
        );
        assert_eq!(primary.cards[1].primary, "60%");
        assert_eq!(
            primary.cards[2].delta.as_ref().map(|d| d.trend),
            Some(DeltaTrend::Negative)
        );
        assert_eq!(secondary.cards.len(), 4);

        let view = ChannelStatsView::new(bundle, &palette);
        assert_eq!(view.charts.len(), 2);

        // Verify remote zoom via stats.loadAsyncGraph(token, x).
        h.fake.enqueue_ok(
            tl_fns::stats::LoadAsyncGraph::CONSTRUCTOR_ID,
            &tl_graph_data(ZOOM_HOURLY_JSON, None),
        );
        let zoomed = h
            .live
            .load_zoom_graph("zoom_growth_tok", 1_700_006_400_000.0)
            .expect("load_zoom_graph");
        assert_eq!(zoomed.chart.x.len(), 3);
        let load_calls = h
            .fake
            .recorded_calls_of(tl_fns::stats::LoadAsyncGraph::CONSTRUCTOR_ID);
        let last_zoom =
            tl_fns::stats::LoadAsyncGraph::from_bytes(load_calls.last().expect("zoom call"))
                .expect("decode zoom call");
        assert_eq!(last_zoom.token, "zoom_growth_tok");
        assert_eq!(last_zoom.x, Some(1_700_006_400_000));

        h.shutdown();
    }

    #[test]
    fn megagroup_message_story_boosts_and_earn_rpcs_convert_faithfully() {
        let h = StatsHarness::start();

        // 1. Megagroup stats
        h.live.set_stats_target_peer(h.supergroup_peer);
        h.fake.enqueue_ok(
            tl_fns::stats::GetMegagroupStats::CONSTRUCTOR_ID,
            &tl_enums::stats::MegagroupStats::MegagroupStats(tl_types::stats::MegagroupStats {
                period: tl_enums::StatsDateRangeDays::StatsDateRangeDays(
                    tl_types::StatsDateRangeDays {
                        min_date: 1_700_006_400,
                        max_date: 1_701_129_600,
                    },
                ),
                members: tl_abs_prev(3_000.0, 2_800.0),
                messages: tl_abs_prev(9_000.0, 8_100.0),
                viewers: tl_abs_prev(1_200.0, 1_100.0),
                posters: tl_abs_prev(400.0, 350.0),
                growth_graph: tl_graph_data(GROWTH_JSON, None),
                members_graph: tl_graph_error("N/A"),
                new_members_by_source_graph: tl_graph_error("N/A"),
                languages_graph: tl_graph_error("N/A"),
                messages_graph: tl_graph_error("N/A"),
                actions_graph: tl_graph_error("N/A"),
                top_hours_graph: tl_graph_error("N/A"),
                weekdays_graph: tl_graph_error("N/A"),
                top_posters: vec![tl_enums::StatsGroupTopPoster::StatsGroupTopPoster(
                    tl_types::StatsGroupTopPoster {
                        user_id: 501,
                        messages: 180,
                        avg_chars: 64,
                    },
                )],
                top_admins: vec![tl_enums::StatsGroupTopAdmin::StatsGroupTopAdmin(
                    tl_types::StatsGroupTopAdmin {
                        user_id: 502,
                        deleted: 12,
                        kicked: 3,
                        banned: 5,
                    },
                )],
                top_inviters: vec![tl_enums::StatsGroupTopInviter::StatsGroupTopInviter(
                    tl_types::StatsGroupTopInviter {
                        user_id: 503,
                        invitations: 9,
                    },
                )],
                users: vec![
                    tl_user(501, "PosterOne", "poster1", false),
                    tl_user(502, "AdminOne", "admin1", false),
                    tl_user(503, "InviterOne", "inviter1", false),
                ],
            }),
        );
        let mg = h.live.supergroup_statistics().expect("supergroup_stats");
        assert_eq!(mg.peer.title, "Rust Forum");
        assert_eq!(mg.lists.top_senders[0].sent_message_count, 180);
        assert_eq!(mg.lists.top_administrators[0].banned_user_count, 3);
        assert_eq!(mg.lists.top_administrators[0].restricted_user_count, 5);
        assert_eq!(mg.lists.top_inviters[0].added_member_count, 9);
        assert_eq!(
            mg.users.get(&501).map(|u| u.title.as_str()),
            Some("PosterOne")
        );

        // 2. Message stats & Story stats
        h.live.set_stats_target_peer(h.channel_peer);
        h.fake.enqueue_ok(
            tl_fns::stats::GetMessageStats::CONSTRUCTOR_ID,
            &tl_enums::stats::MessageStats::MessageStats(tl_types::stats::MessageStats {
                views_graph: tl_graph_data(GROWTH_JSON, None),
                reactions_by_emotion_graph: tl_graph_error("NONE"),
            }),
        );
        h.fake.enqueue_ok(
            tl_fns::channels::GetMessages::CONSTRUCTOR_ID,
            &tl_enums::messages::Messages::ChannelMessages(tl_types::messages::ChannelMessages {
                inexact: false,
                pts: 1,
                count: 1,
                offset_id_offset: None,
                messages: vec![tl_enums::Message::Message(tl_types::Message {
                    out: false,
                    mentioned: false,
                    media_unread: false,
                    silent: false,
                    post: true,
                    from_scheduled: false,
                    legacy: false,
                    edit_hide: false,
                    pinned: false,
                    noforwards: false,
                    invert_media: false,
                    offline: false,
                    video_processing_pending: false,
                    paid_suggested_post_stars: false,
                    paid_suggested_post_ton: false,
                    id: 77,
                    from_id: None,
                    from_boosts_applied: None,
                    from_rank: None,
                    peer_id: tl_enums::Peer::Channel(tl_types::PeerChannel {
                        channel_id: 910_001,
                    }),
                    saved_peer_id: None,
                    fwd_from: None,
                    via_bot_id: None,
                    via_business_bot_id: None,
                    guestchat_via_from: None,
                    reply_to: None,
                    date: 1_700_500_000,
                    message: "Launch post!".to_owned(),
                    media: None,
                    reply_markup: None,
                    entities: None,
                    views: Some(2500),
                    forwards: Some(100),
                    replies: None,
                    edit_date: None,
                    post_author: None,
                    grouped_id: None,
                    reactions: None,
                    restriction_reason: None,
                    ttl_period: None,
                    quick_reply_shortcut_id: None,
                    effect: None,
                    factcheck: None,
                    report_delivery_until_date: None,
                    paid_message_stars: None,
                    suggested_post: None,
                    schedule_repeat_period: None,
                    summary_from_language: None,
                    rich_message: None,
                })],
                topics: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
            }),
        );
        h.fake.enqueue_ok(
            tl_fns::stats::GetMessagePublicForwards::CONSTRUCTOR_ID,
            &tl_enums::stats::PublicForwards::PublicForwards(tl_types::stats::PublicForwards {
                count: 25,
                forwards: Vec::new(),
                next_offset: None,
                chats: Vec::new(),
                users: Vec::new(),
            }),
        );
        let msg_stats = h.live.message_statistics(77).expect("message_statistics");
        assert_eq!(msg_stats.stats.views, 2500);
        assert_eq!(msg_stats.stats.public_forwards, 25);
        assert_eq!(msg_stats.stats.private_forwards, 75);
        assert_eq!(msg_stats.post.snippet, "Launch post!");

        h.fake.enqueue_ok(
            tl_fns::stats::GetStoryStats::CONSTRUCTOR_ID,
            &tl_enums::stats::StoryStats::StoryStats(tl_types::stats::StoryStats {
                views_graph: tl_graph_data(GROWTH_JSON, None),
                reactions_by_emotion_graph: tl_graph_error("NONE"),
            }),
        );
        h.fake.enqueue_ok(
            tl_fns::stories::GetStoriesByID::CONSTRUCTOR_ID,
            &tl_enums::stories::Stories::Stories(tl_types::stories::Stories {
                count: 0,
                stories: Vec::new(),
                pinned_to_top: None,
                chats: Vec::new(),
                users: Vec::new(),
            }),
        );
        h.fake.enqueue_ok(
            tl_fns::stats::GetStoryPublicForwards::CONSTRUCTOR_ID,
            &tl_enums::stats::PublicForwards::PublicForwards(tl_types::stats::PublicForwards {
                count: 4,
                forwards: Vec::new(),
                next_offset: None,
                chats: Vec::new(),
                users: Vec::new(),
            }),
        );
        let story_stats = h.live.story_statistics(12).expect("story_statistics");
        assert!(story_stats.post.is_story);
        assert_eq!(story_stats.stats.public_forwards, 4);

        // 3. Boosts status + list pagination
        h.fake.enqueue_ok(
            tl_fns::premium::GetBoostsStatus::CONSTRUCTOR_ID,
            &tl_enums::premium::BoostsStatus::BoostsStatus(tl_types::premium::BoostsStatus {
                my_boost: true,
                level: 4,
                current_level_boosts: 15,
                boosts: 19,
                gift_boosts: Some(4),
                next_level_boosts: Some(25),
                premium_audience: Some(tl_enums::StatsPercentValue::StatsPercentValue(
                    tl_types::StatsPercentValue {
                        part: 250.0,
                        total: 5000.0,
                    },
                )),
                boost_url: "https://t.me/boost/tech_news".to_owned(),
                prepaid_giveaways: Some(vec![tl_enums::PrepaidGiveaway::PrepaidStarsGiveaway(
                    tl_types::PrepaidStarsGiveaway {
                        id: 700,
                        stars: 5000,
                        quantity: 10,
                        boosts: 12,
                        date: 1_700_000_000,
                    },
                )]),
                my_boost_slots: Some(vec![1]),
            }),
        );
        let boost_list_resp =
            tl_enums::premium::BoostsList::BoostsList(tl_types::premium::BoostsList {
                count: 19,
                boosts: vec![tl_enums::Boost::Boost(tl_types::Boost {
                    gift: false,
                    giveaway: false,
                    unclaimed: false,
                    id: "b1".to_owned(),
                    user_id: Some(501),
                    giveaway_msg_id: None,
                    date: 1_700_000_000,
                    expires: 1_702_592_000,
                    used_gift_slug: None,
                    multiplier: Some(1),
                    stars: None,
                })],
                next_offset: Some("next_b".to_owned()),
                users: vec![tl_user(501, "PosterOne", "poster1", false)],
            });
        h.fake.enqueue_ok(
            tl_fns::premium::GetBoostsList::CONSTRUCTOR_ID,
            &boost_list_resp,
        );
        h.fake.enqueue_ok(
            tl_fns::premium::GetBoostsList::CONSTRUCTOR_ID,
            &tl_enums::premium::BoostsList::BoostsList(tl_types::premium::BoostsList {
                count: 0,
                boosts: Vec::new(),
                next_offset: None,
                users: Vec::new(),
            }),
        );
        let boosts = h.live.boost_status().expect("boost_status");
        assert_eq!(boosts.status.overview.level, 4);
        assert_eq!(boosts.status.overview.boost_count, 19);
        assert!((boosts.status.overview.premium_member_percentage - 5.0).abs() < 1e-6);
        assert_eq!(boosts.status.prepaid_giveaway.len(), 1);
        assert_eq!(boosts.status.first_slice_boosts.list.len(), 1);
        assert_eq!(boosts.status.first_slice_boosts.token.next, "next_b");

        h.fake.enqueue_ok(
            tl_fns::premium::GetBoostsList::CONSTRUCTOR_ID,
            &tl_enums::premium::BoostsList::BoostsList(tl_types::premium::BoostsList {
                count: 19,
                boosts: Vec::new(),
                next_offset: None,
                users: Vec::new(),
            }),
        );
        let more_boosts = h
            .live
            .load_more_boosts(&BoostsOffsetToken {
                next: "next_b".to_owned(),
                gifts: false,
            })
            .expect("load_more_boosts");
        assert!(more_boosts.all_loaded);

        // 4. Channel Earn (TON + Stars) and Bot Earn
        h.fake.enqueue_ok(
            tl_fns::payments::GetStarsRevenueStats::CONSTRUCTOR_ID,
            &tl_enums::payments::StarsRevenueStats::StarsRevenueStats(
                tl_types::payments::StarsRevenueStats {
                    top_hours_graph: Some(tl_graph_data(GROWTH_JSON, None)),
                    revenue_graph: tl_graph_data(TON_REVENUE_JSON, None),
                    status: tl_enums::StarsRevenueStatus::StarsRevenueStatus(
                        tl_types::StarsRevenueStatus {
                            withdrawal_enabled: true,
                            current_balance: tl_enums::StarsAmount::StarsTonAmount(
                                tl_types::StarsTonAmount {
                                    amount: 12_500_000_000,
                                },
                            ),
                            available_balance: tl_enums::StarsAmount::StarsTonAmount(
                                tl_types::StarsTonAmount {
                                    amount: 8_000_000_000,
                                },
                            ),
                            overall_revenue: tl_enums::StarsAmount::StarsTonAmount(
                                tl_types::StarsTonAmount {
                                    amount: 50_000_000_000,
                                },
                            ),
                            next_withdrawal_at: None,
                        },
                    ),
                    usd_rate: 5.5,
                },
            ),
        );
        h.fake.enqueue_ok(
            tl_fns::payments::GetStarsRevenueStats::CONSTRUCTOR_ID,
            &tl_enums::payments::StarsRevenueStats::StarsRevenueStats(
                tl_types::payments::StarsRevenueStats {
                    top_hours_graph: None,
                    revenue_graph: tl_graph_data(STARS_REVENUE_JSON, None),
                    status: tl_enums::StarsRevenueStatus::StarsRevenueStatus(
                        tl_types::StarsRevenueStatus {
                            withdrawal_enabled: true,
                            current_balance: tl_enums::StarsAmount::StarsAmount(
                                tl_types::StarsAmount {
                                    amount: 4_200,
                                    nanos: 0,
                                },
                            ),
                            available_balance: tl_enums::StarsAmount::StarsAmount(
                                tl_types::StarsAmount {
                                    amount: 3_100,
                                    nanos: 0,
                                },
                            ),
                            overall_revenue: tl_enums::StarsAmount::StarsAmount(
                                tl_types::StarsAmount {
                                    amount: 9_800,
                                    nanos: 0,
                                },
                            ),
                            next_withdrawal_at: None,
                        },
                    ),
                    usd_rate: 0.013,
                },
            ),
        );
        let ton_tx_resp =
            tl_enums::payments::StarsStatus::StarsStatus(tl_types::payments::StarsStatus {
                balance: tl_enums::StarsAmount::StarsTonAmount(tl_types::StarsTonAmount {
                    amount: 12_500_000_000,
                }),
                subscriptions: None,
                subscriptions_next_offset: None,
                subscriptions_missing_balance: None,
                history: Some(vec![tl_enums::StarsTransaction::StarsTransaction(
                    tl_types::StarsTransaction {
                        refund: false,
                        pending: false,
                        failed: false,
                        gift: false,
                        reaction: false,
                        stargift_upgrade: false,
                        business_transfer: false,
                        stargift_resale: false,
                        posts_search: false,
                        stargift_prepaid_upgrade: false,
                        stargift_drop_original_details: false,
                        phonegroup_message: false,
                        stargift_auction_bid: false,
                        offer: false,
                        id: "tx_ton_1".to_owned(),
                        amount: tl_enums::StarsAmount::StarsTonAmount(tl_types::StarsTonAmount {
                            amount: 3_000_000_000,
                        }),
                        date: 1_700_100_000,
                        peer: tl_enums::StarsTransactionPeer::Ads,
                        title: None,
                        description: None,
                        photo: None,
                        transaction_date: None,
                        transaction_url: None,
                        bot_payload: None,
                        msg_id: None,
                        extended_media: None,
                        subscription_period: None,
                        giveaway_post_id: None,
                        stargift: None,
                        floodskip_number: None,
                        starref_commission_permille: None,
                        starref_peer: None,
                        starref_amount: None,
                        paid_messages: None,
                        premium_gift_months: None,
                        ads_proceeds_from_date: Some(1_700_006_400),
                        ads_proceeds_to_date: Some(1_700_092_800),
                    },
                )]),
                next_offset: None,
                chats: Vec::new(),
                users: Vec::new(),
            });
        h.fake.enqueue_ok(
            tl_fns::payments::GetStarsTransactions::CONSTRUCTOR_ID,
            &ton_tx_resp,
        );
        h.fake.enqueue_ok(
            tl_fns::payments::GetStarsRevenueAdsAccountUrl::CONSTRUCTOR_ID,
            &tl_enums::payments::StarsRevenueAdsAccountUrl::StarsRevenueAdsAccountUrl(
                tl_types::payments::StarsRevenueAdsAccountUrl {
                    url: "https://ads.telegram.org".to_owned(),
                },
            ),
        );
        h.fake.enqueue_ok(
            tl_fns::payments::GetStarsTransactions::CONSTRUCTOR_ID,
            &tl_enums::payments::StarsStatus::StarsStatus(tl_types::payments::StarsStatus {
                balance: tl_enums::StarsAmount::StarsAmount(tl_types::StarsAmount {
                    amount: 4_200,
                    nanos: 0,
                }),
                subscriptions: None,
                subscriptions_next_offset: None,
                subscriptions_missing_balance: None,
                history: Some(Vec::new()),
                next_offset: None,
                chats: Vec::new(),
                users: Vec::new(),
            }),
        );

        let earn = h.live.channel_earn().expect("channel_earn");
        assert!(earn.can_view_currency);
        assert!(earn.can_view_credits);
        assert_eq!(
            earn.ton.revenue_graph.chart.currency,
            StatisticalCurrency::Ton
        );
        assert_eq!(earn.ton.available_balance.credits_type(), CreditsType::Ton);
        assert_eq!(earn.ton.available_balance.whole(), 8);
        assert_eq!(earn.ton.first_history_slice.list.len(), 1);
        assert_eq!(
            earn.ton.first_history_slice.list[0].title,
            "Proceeds from Telegram Ads"
        );
        assert_eq!(earn.credits.overall_revenue.whole(), 9_800);
        assert_eq!(earn.credits.buy_ads_url, "https://ads.telegram.org");

        // Bot Earn
        h.live.set_stats_target_peer(h.bot_peer);
        h.fake.enqueue_ok(
            tl_fns::payments::GetStarsRevenueStats::CONSTRUCTOR_ID,
            &tl_enums::payments::StarsRevenueStats::StarsRevenueStats(
                tl_types::payments::StarsRevenueStats {
                    top_hours_graph: None,
                    revenue_graph: tl_graph_data(STARS_REVENUE_JSON, None),
                    status: tl_enums::StarsRevenueStatus::StarsRevenueStatus(
                        tl_types::StarsRevenueStatus {
                            withdrawal_enabled: true,
                            current_balance: tl_enums::StarsAmount::StarsAmount(
                                tl_types::StarsAmount {
                                    amount: 7_500,
                                    nanos: 0,
                                },
                            ),
                            available_balance: tl_enums::StarsAmount::StarsAmount(
                                tl_types::StarsAmount {
                                    amount: 6_000,
                                    nanos: 0,
                                },
                            ),
                            overall_revenue: tl_enums::StarsAmount::StarsAmount(
                                tl_types::StarsAmount {
                                    amount: 20_000,
                                    nanos: 0,
                                },
                            ),
                            next_withdrawal_at: Some(1_701_500_000),
                        },
                    ),
                    usd_rate: 0.013,
                },
            ),
        );
        h.fake.enqueue_ok(
            tl_fns::payments::GetStarsRevenueAdsAccountUrl::CONSTRUCTOR_ID,
            &tl_enums::payments::StarsRevenueAdsAccountUrl::StarsRevenueAdsAccountUrl(
                tl_types::payments::StarsRevenueAdsAccountUrl {
                    url: "https://ads.telegram.org/bot".to_owned(),
                },
            ),
        );
        let empty_stars_status =
            tl_enums::payments::StarsStatus::StarsStatus(tl_types::payments::StarsStatus {
                balance: tl_enums::StarsAmount::StarsAmount(tl_types::StarsAmount {
                    amount: 7_500,
                    nanos: 0,
                }),
                subscriptions: None,
                subscriptions_next_offset: None,
                subscriptions_missing_balance: None,
                history: Some(Vec::new()),
                next_offset: None,
                chats: Vec::new(),
                users: Vec::new(),
            });
        for _ in 0..3 {
            h.fake.enqueue_ok(
                tl_fns::payments::GetStarsTransactions::CONSTRUCTOR_ID,
                &empty_stars_status,
            );
        }
        let bot = h.live.bot_earn().expect("bot_earn");
        assert_eq!(bot.peer.title, "StatsBot");
        assert_eq!(bot.credits.available_balance.whole(), 6_000);
        assert_eq!(bot.credits.next_withdrawal_at, 1_701_500_000);

        let ctors = h.fake.recorded_ctors();
        assert!(ctors.contains(&tl_fns::stats::GetMegagroupStats::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::stats::GetMessageStats::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::stats::GetStoryStats::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::premium::GetBoostsStatus::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::premium::GetBoostsList::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::payments::GetStarsRevenueStats::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::payments::GetStarsTransactions::CONSTRUCTOR_ID));

        h.shutdown();
    }
}

mod stories_tests {
    use std::collections::VecDeque;
    use std::process::Command;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use tg_api::{DcId, InvocationError, RpcClient};
    use tg_data::{FullStoryId, PeerId as DataPeerId, ReactionId, UserId as DataUserId};
    use tg_session::{SessionRuntime, SessionRuntimeConfig};
    use tg_stories::{
        MARK_AS_READ_DELAY_MS, PEER_MAX_IDS_POLL_DELAY_MS, StoriesContext, StoriesStore,
        StorySourcesList, StoryViewerController, ViewerConfig,
    };
    use tg_tl::{
        Deserializable, Identifiable, RemoteCall, Serializable, enums as tl_enums,
        functions as tl_fns, types as tl_types,
    };
    use tg_video::PlaybackOptions;
    use tokio::sync::broadcast;

    use tg_app::live::LiveSession;
    use tg_app::stories::backend::{StoriesBackend, StoryMediaAsset};
    use tg_app::stories::strip::StoriesStrip;

    const TEST_TIMEOUT: Duration = Duration::from_secs(10);
    const NOW_SECS: i32 = 1_750_000_000;

    type RecordedCall = (u32, Vec<u8>);
    type QueuedResponse = (u32, Result<Vec<u8>, InvocationError>);
    type DynamicHandler =
        Arc<dyn Fn(u32, &[u8]) -> Option<Result<Vec<u8>, InvocationError>> + Send + Sync>;

    #[derive(Clone)]
    struct FakeStoriesRpc {
        recorded: Arc<Mutex<Vec<RecordedCall>>>,
        responses: Arc<Mutex<VecDeque<QueuedResponse>>>,
        dynamic: Arc<Mutex<Option<DynamicHandler>>>,
        updates_tx: broadcast::Sender<tl_enums::Updates>,
    }

    impl FakeStoriesRpc {
        fn new() -> Self {
            let (updates_tx, _) = broadcast::channel(64);
            Self {
                recorded: Arc::new(Mutex::new(Vec::new())),
                responses: Arc::new(Mutex::new(VecDeque::new())),
                dynamic: Arc::new(Mutex::new(None)),
                updates_tx,
            }
        }

        fn enqueue_ok<T: Serializable>(&self, expected_ctor: u32, resp: &T) {
            self.responses
                .lock()
                .expect("lock responses")
                .push_back((expected_ctor, Ok(resp.to_bytes())));
        }

        fn set_dynamic_handler(&self, handler: DynamicHandler) {
            *self.dynamic.lock().expect("lock dynamic") = Some(handler);
        }

        fn push_update(&self, updates: tl_enums::Updates) {
            let _ = self.updates_tx.send(updates);
        }

        fn recorded_ctors(&self) -> Vec<u32> {
            self.recorded
                .lock()
                .expect("lock recorded")
                .iter()
                .map(|(c, _)| *c)
                .collect()
        }

        fn recorded_calls_of(&self, ctor: u32) -> Vec<Vec<u8>> {
            self.recorded
                .lock()
                .expect("lock recorded")
                .iter()
                .filter(|(c, _)| *c == ctor)
                .map(|(_, b)| b.clone())
                .collect()
        }
    }

    impl RpcClient for FakeStoriesRpc {
        type Error = InvocationError;

        async fn invoke<R: RemoteCall + Send + Sync>(
            &self,
            req: &R,
        ) -> Result<R::Return, Self::Error> {
            self.invoke_in_dc(2, req).await
        }

        async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
            &self,
            _dc_id: DcId,
            req: &R,
        ) -> Result<R::Return, Self::Error> {
            let bytes = req.to_bytes();
            let ctor = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            self.recorded
                .lock()
                .expect("lock recorded")
                .push((ctor, bytes.clone()));

            if let Some(handler) = self.dynamic.lock().expect("lock dynamic").clone()
                && let Some(res) = handler(ctor, &bytes)
            {
                let raw = res?;
                return R::Return::from_bytes(&raw)
                    .map_err(|e| InvocationError::Deserialize(e.to_string()));
            }

            let next = {
                let mut q = self.responses.lock().expect("lock responses");
                q.iter()
                    .position(|(c, _)| *c == ctor)
                    .and_then(|pos| q.remove(pos).map(|(_, r)| r))
            };
            let raw = next.unwrap_or(Err(InvocationError::Dropped))?;
            R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
        }

        fn main_dc_id(&self) -> DcId {
            2
        }

        fn set_main_dc_id(&self, _dc_id: DcId) {}

        fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
            Some(self.updates_tx.subscribe())
        }
    }

    fn make_tl_user(
        id: i64,
        first_name: &str,
        username: &str,
        is_self: bool,
        stories_hidden: bool,
    ) -> tl_types::User {
        tl_types::User {
            self_: is_self,
            contact: !is_self,
            mutual_contact: false,
            deleted: false,
            bot: false,
            bot_chat_history: false,
            bot_nochats: false,
            verified: false,
            restricted: false,
            min: false,
            bot_inline_geo: false,
            support: false,
            scam: false,
            apply_min_photo: false,
            fake: false,
            bot_attach_menu: false,
            premium: true,
            attach_menu_enabled: false,
            bot_can_edit: false,
            close_friend: false,
            stories_hidden,
            stories_unavailable: false,
            contact_require_premium: false,
            bot_business: false,
            bot_has_main_app: false,
            bot_forum_view: false,
            bot_forum_can_manage_topics: false,
            bot_can_manage_bots: false,
            bot_guestchat: false,
            bot_guard: false,
            bot_forum_sidebar: false,
            id,
            access_hash: Some(id * 100 + 9),
            first_name: Some(first_name.to_owned()),
            last_name: None,
            username: Some(username.to_owned()),
            phone: None,
            photo: None,
            status: None,
            bot_info_version: None,
            restriction_reason: None,
            bot_inline_placeholder: None,
            lang_code: None,
            emoji_status: None,
            usernames: None,
            stories_max_id: None,
            color: None,
            profile_color: None,
            bot_active_users: None,
            bot_verification_icon: None,
            send_paid_messages_stars: None,
            linked_community_id: None,
        }
    }

    fn make_photo_story_item(
        id: i32,
        date: i32,
        expire_date: i32,
        photo_id: i64,
        caption: &str,
        pinned: bool,
    ) -> tl_enums::StoryItem {
        tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
            pinned,
            public: true,
            close_friends: false,
            min: false,
            noforwards: false,
            edited: false,
            contacts: false,
            selected_contacts: false,
            out: false,
            id,
            date,
            from_id: None,
            fwd_from: None,
            expire_date,
            caption: Some(caption.to_owned()),
            entities: None,
            media: Box::new(tl_enums::MessageMedia::Photo(tl_types::MessageMediaPhoto {
                spoiler: false,
                live_photo: false,
                photo: Some(tl_enums::Photo::Photo(tl_types::Photo {
                    has_stickers: false,
                    id: photo_id,
                    access_hash: photo_id * 13,
                    file_reference: vec![9, 8, 7],
                    date,
                    sizes: vec![tl_enums::PhotoSize::PhotoSize(tl_types::PhotoSize {
                        r#type: "y".to_owned(),
                        w: 320,
                        h: 568,
                        size: 1024,
                    })],
                    video_sizes: None,
                    dc_id: 2,
                })),
                ttl_seconds: None,
                video: None,
            })),
            media_areas: None,
            privacy: None,
            views: None,
            sent_reaction: None,
            albums: None,
            music: None,
        })
    }

    fn make_video_story_item(
        id: i32,
        date: i32,
        expire_date: i32,
        doc_id: i64,
        size: i64,
    ) -> tl_enums::StoryItem {
        tl_enums::StoryItem::StoryItem(tl_types::StoryItem {
            pinned: false,
            public: true,
            close_friends: false,
            min: false,
            noforwards: false,
            edited: false,
            contacts: false,
            selected_contacts: false,
            out: false,
            id,
            date,
            from_id: None,
            fwd_from: None,
            expire_date,
            caption: Some("Video story".to_owned()),
            entities: None,
            media: Box::new(tl_enums::MessageMedia::Document(
                tl_types::MessageMediaDocument {
                    nopremium: false,
                    spoiler: false,
                    video: true,
                    round: false,
                    voice: false,
                    document: Some(tl_enums::Document::Document(tl_types::Document {
                        id: doc_id,
                        access_hash: doc_id * 17,
                        file_reference: vec![4, 5, 6],
                        date,
                        mime_type: "video/mp4".to_owned(),
                        size,
                        thumbs: None,
                        video_thumbs: None,
                        dc_id: 2,
                        attributes: vec![tl_enums::DocumentAttribute::Video(
                            tl_types::DocumentAttributeVideo {
                                round_message: false,
                                supports_streaming: true,
                                nosound: true,
                                duration: 2.0,
                                w: 180,
                                h: 320,
                                preload_prefix_size: None,
                                video_start_ts: None,
                                video_codec: None,
                            },
                        )],
                    })),
                    alt_documents: None,
                    video_cover: None,
                    video_timestamp: None,
                    ttl_seconds: None,
                },
            )),
            media_areas: None,
            privacy: None,
            views: None,
            sent_reaction: None,
            albums: None,
            music: None,
        })
    }

    fn start_stories_harness() -> (
        FakeStoriesRpc,
        Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
        LiveSession,
        DataPeerId,
        DataPeerId,
    ) {
        let self_uid = 710_001i64;
        let bob_uid = 710_002i64;
        let fake = FakeStoriesRpc::new();
        fake.enqueue_ok(
            tl_fns::users::GetUsers::CONSTRUCTOR_ID,
            &vec![tl_enums::User::User(make_tl_user(
                self_uid,
                "Alice",
                "alice_stories",
                true,
                false,
            ))],
        );
        fake.enqueue_ok(
            tl_fns::updates::GetState::CONSTRUCTOR_ID,
            &tl_enums::updates::State::State(tl_types::updates::State {
                pts: 10,
                qts: 0,
                date: NOW_SECS,
                seq: 0,
                unread_count: 0,
            }),
        );
        let cfg = SessionRuntimeConfig {
            init_updates_state: true,
            run_get_difference_on_start: false,
            load_initial_dialogs: false,
            fetch_self_user_on_start: true,
            ..SessionRuntimeConfig::default()
        };
        let runtime = Arc::new(
            SessionRuntime::start(
                DataUserId::from_tl(self_uid),
                tg_app::live::ErasedRpcClient::new(fake.clone()),
                cfg,
            )
            .expect("start runtime"),
        );
        runtime
            .tokio_handle()
            .block_on(async { tokio::time::timeout(TEST_TIMEOUT, runtime.wait_ready()).await })
            .expect("wait_ready timeout")
            .expect("ready");
        let live = LiveSession::new(Arc::clone(&runtime), 1);
        (
            fake,
            runtime,
            live,
            DataPeerId::from_user(DataUserId::from_tl(self_uid)),
            DataPeerId::from_user(DataUserId::from_tl(bob_uid)),
        )
    }

    fn shutdown_harness(
        live: LiveSession,
        runtime: Arc<SessionRuntime<tg_app::live::ErasedRpcClient>>,
    ) {
        drop(live);
        if let Ok(rt) = Arc::try_unwrap(runtime) {
            rt.shutdown();
        }
    }

    #[test]
    fn get_all_stories_pagination_fills_strip_and_read_stories_batches() {
        let (fake, runtime, mut live, self_peer, bob_peer) = start_stories_harness();
        let bob_uid = bob_peer.bare() as i64;
        let hidden_uid = 710_003i64;
        let hidden_peer = DataPeerId::from_user(DataUserId::from_tl(hidden_uid));

        let mut store = StoriesStore::new(self_peer);

        // Page 1 of NotHidden (`has_more: true`, state = "page1_token").
        fake.enqueue_ok(
            tl_fns::stories::GetAllStories::CONSTRUCTOR_ID,
            &tl_enums::stories::AllStories::AllStories(tl_types::stories::AllStories {
                has_more: true,
                count: 2,
                state: "page1_token".to_owned(),
                peer_stories: vec![tl_enums::PeerStories::PeerStories(tl_types::PeerStories {
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: bob_uid }),
                    max_read_id: Some(9),
                    stories: vec![
                        make_photo_story_item(
                            10,
                            NOW_SECS - 300,
                            NOW_SECS + 3600,
                            9010,
                            "Bob story 10",
                            false,
                        ),
                        make_photo_story_item(
                            11,
                            NOW_SECS - 120,
                            NOW_SECS + 3600,
                            9011,
                            "Bob story 11",
                            false,
                        ),
                    ],
                })],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(make_tl_user(
                    bob_uid, "Bob", "bob_s", false, false,
                ))],
                stealth_mode: tl_enums::StoriesStealthMode::StoriesStealthMode(
                    tl_types::StoriesStealthMode {
                        active_until_date: None,
                        cooldown_until_date: None,
                    },
                ),
            }),
        );

        store.load_more(StorySourcesList::NotHidden);
        let acts = store.take_actions();
        live.execute_actions(&mut store, acts, NOW_SECS);

        // Strip is populated with Bob's 2 unread stories.
        assert_eq!(store.sources(StorySourcesList::NotHidden).len(), 1);
        assert_eq!(store.sources(StorySourcesList::NotHidden)[0].id, bob_peer);
        assert_eq!(store.sources(StorySourcesList::NotHidden)[0].count, 2);
        assert_eq!(
            store.sources(StorySourcesList::NotHidden)[0].unread_count,
            2
        );
        assert!(!store.sources_loaded(StorySourcesList::NotHidden));

        let mut strip = StoriesStrip::new();
        strip.sync_from_store(&store, &live);
        assert_eq!(strip.bar_state().content().elements.len(), 1);
        assert_eq!(strip.bar_state().content().elements[0].id, bob_peer.raw());

        // Page 2 of NotHidden verifies `next = true` + `state = Some("page1_token")`.
        fake.enqueue_ok(
            tl_fns::stories::GetAllStories::CONSTRUCTOR_ID,
            &tl_enums::stories::AllStories::AllStories(tl_types::stories::AllStories {
                has_more: false,
                count: 1,
                state: "page2_done".to_owned(),
                peer_stories: Vec::new(),
                chats: Vec::new(),
                users: Vec::new(),
                stealth_mode: tl_enums::StoriesStealthMode::StoriesStealthMode(
                    tl_types::StoriesStealthMode {
                        active_until_date: None,
                        cooldown_until_date: None,
                    },
                ),
            }),
        );
        store.load_more(StorySourcesList::NotHidden);
        let acts2 = store.take_actions();
        live.execute_actions(&mut store, acts2, NOW_SECS);
        assert!(store.sources_loaded(StorySourcesList::NotHidden));

        let get_all_calls = fake.recorded_calls_of(tl_fns::stories::GetAllStories::CONSTRUCTOR_ID);
        assert_eq!(get_all_calls.len(), 2);
        let req1 = tl_fns::stories::GetAllStories::from_bytes(&get_all_calls[0]).expect("req1");
        assert!(!req1.next);
        assert!(!req1.hidden);
        assert_eq!(req1.state, None);
        let req2 = tl_fns::stories::GetAllStories::from_bytes(&get_all_calls[1]).expect("req2");
        assert!(req2.next);
        assert!(!req2.hidden);
        assert_eq!(req2.state.as_deref(), Some("page1_token"));

        // Hidden strip loads with `hidden = true`.
        fake.enqueue_ok(
            tl_fns::stories::GetAllStories::CONSTRUCTOR_ID,
            &tl_enums::stories::AllStories::AllStories(tl_types::stories::AllStories {
                has_more: false,
                count: 1,
                state: "hidden_done".to_owned(),
                peer_stories: vec![tl_enums::PeerStories::PeerStories(tl_types::PeerStories {
                    peer: tl_enums::Peer::User(tl_types::PeerUser {
                        user_id: hidden_uid,
                    }),
                    max_read_id: None,
                    stories: vec![make_photo_story_item(
                        50,
                        NOW_SECS - 60,
                        NOW_SECS + 3600,
                        9050,
                        "Hidden story",
                        false,
                    )],
                })],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(make_tl_user(
                    hidden_uid, "Carol", "carol_h", false, true,
                ))],
                stealth_mode: tl_enums::StoriesStealthMode::StoriesStealthMode(
                    tl_types::StoriesStealthMode {
                        active_until_date: None,
                        cooldown_until_date: None,
                    },
                ),
            }),
        );
        store.load_more(StorySourcesList::Hidden);
        let hidden_acts = store.take_actions();
        live.execute_actions(&mut store, hidden_acts, NOW_SECS);
        assert_eq!(store.sources(StorySourcesList::Hidden).len(), 1);
        assert_eq!(store.sources(StorySourcesList::Hidden)[0].id, hidden_peer);

        // Reading two stories of Bob within the 3 s debounce batches into ONE `stories.readStories(max_id = 11)`.
        let mut viewer = StoryViewerController::new();
        assert!(viewer.show(
            &mut store,
            FullStoryId::new(bob_peer, 10),
            StoriesContext::SourcesList(StorySourcesList::NotHidden),
            ViewerConfig::default(),
            NOW_SECS,
            1_000,
        ));
        store.mark_as_read(FullStoryId::new(bob_peer, 10), NOW_SECS, 1_200);
        store.mark_as_read(FullStoryId::new(bob_peer, 11), NOW_SECS, 1_800);
        // Before 3s debounce expires, no ReadStories RPC yet.
        assert!(
            fake.recorded_calls_of(tl_fns::stories::ReadStories::CONSTRUCTOR_ID)
                .is_empty()
        );

        fake.enqueue_ok(
            tl_fns::stories::ReadStories::CONSTRUCTOR_ID,
            &vec![10i32, 11i32],
        );
        store.tick(NOW_SECS + 4, 1_800 + MARK_AS_READ_DELAY_MS + 10);
        let read_acts = store.take_actions();
        live.execute_actions(&mut store, read_acts, NOW_SECS + 4);

        let read_calls = fake.recorded_calls_of(tl_fns::stories::ReadStories::CONSTRUCTOR_ID);
        assert_eq!(read_calls.len(), 1);
        let read_req = tl_fns::stories::ReadStories::from_bytes(&read_calls[0]).expect("read_req");
        assert_eq!(read_req.max_id, 11);
        assert_eq!(store.read_till(bob_peer), 11);

        shutdown_harness(live, runtime);
    }

    #[test]
    fn stories_rpcs_updates_stream_and_media_loaders() {
        let (fake, runtime, mut live, self_peer, bob_peer) = start_stories_harness();
        let bob_uid = bob_peer.bare() as i64;
        let self_uid = self_peer.bare() as i64;
        let mut store = StoriesStore::new(self_peer);
        live.attach_stories_updates_listener();

        // Seed Bob in Session via GetPeerStories.
        let bob_tl = make_tl_user(bob_uid, "Bob", "bob_user", false, false);
        fake.enqueue_ok(
            tl_fns::stories::GetPeerStories::CONSTRUCTOR_ID,
            &tl_enums::stories::PeerStories::PeerStories(tl_types::stories::PeerStories {
                stories: tl_enums::PeerStories::PeerStories(tl_types::PeerStories {
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: bob_uid }),
                    max_read_id: Some(0),
                    stories: vec![make_photo_story_item(
                        20,
                        NOW_SECS - 100,
                        NOW_SECS + 3600,
                        7020,
                        "Photo story",
                        false,
                    )],
                }),
                chats: Vec::new(),
                users: vec![tl_enums::User::User(bob_tl.clone())],
            }),
        );
        // Seed Bob in Session so resolve_story_input_peer succeeds before first RPC.
        live.apply_users_chats_and_sync_store(
            &mut store,
            &[tl_enums::User::User(bob_tl.clone())],
            &[],
            false,
        );

        store.request_peer_stories(bob_peer);
        let acts = store.take_actions();
        live.execute_actions(&mut store, acts, NOW_SECS);
        assert!(store.lookup(FullStoryId::new(bob_peer, 20)).is_ok());

        // 1. GetStoriesByID
        fake.enqueue_ok(
            tl_fns::stories::GetStoriesByID::CONSTRUCTOR_ID,
            &tl_enums::stories::Stories::Stories(tl_types::stories::Stories {
                count: 1,
                stories: vec![make_photo_story_item(
                    21,
                    NOW_SECS - 8000,
                    NOW_SECS - 10, // expired but in_profile = true -> triggers incrementStoryViews!
                    7021,
                    "Expired profile story",
                    true,
                )],
                pinned_to_top: None,
                chats: Vec::new(),
                users: Vec::new(),
            }),
        );
        store.resolve(FullStoryId::new(bob_peer, 21), true);
        store.flush_resolve_requests();
        let acts = store.take_actions();
        live.execute_actions(&mut store, acts, NOW_SECS);
        assert!(store.lookup(FullStoryId::new(bob_peer, 21)).is_ok());

        // 2. IncrementStoryViews batching for expired in-profile story
        fake.enqueue_ok(tl_fns::stories::IncrementStoryViews::CONSTRUCTOR_ID, &true);
        store.mark_as_read(FullStoryId::new(bob_peer, 21), NOW_SECS, 10_000);
        store.tick(NOW_SECS + 6, 16_000);
        let acts = store.take_actions();
        live.execute_actions(&mut store, acts, NOW_SECS + 6);
        assert!(
            fake.recorded_ctors()
                .contains(&tl_fns::stories::IncrementStoryViews::CONSTRUCTOR_ID)
        );

        // 3. GetPeerMaxIDs polling
        fake.enqueue_ok(
            tl_fns::stories::GetPeerMaxIDs::CONSTRUCTOR_ID,
            &vec![tl_enums::RecentStory::RecentStory(tl_types::RecentStory {
                live: false,
                max_id: Some(25),
            })],
        );
        store.schedule_peer_max_id_poll(bob_peer, 20_000);
        store.tick(NOW_SECS + 7, 20_000 + PEER_MAX_IDS_POLL_DELAY_MS + 10);
        let acts = store.take_actions();
        live.execute_actions(&mut store, acts, NOW_SECS + 7);
        assert!(
            fake.recorded_ctors()
                .contains(&tl_fns::stories::GetPeerMaxIDs::CONSTRUCTOR_ID)
        );

        // 4. SendReaction
        fake.enqueue_ok(
            tl_fns::stories::SendReaction::CONSTRUCTOR_ID,
            &tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: NOW_SECS,
                seq: 0,
            }),
        );
        store.send_reaction(
            FullStoryId::new(bob_peer, 20),
            Some(ReactionId::Emoji("\u{1F525}".to_owned())),
        );
        let acts = store.take_actions();
        live.execute_actions(&mut store, acts, NOW_SECS);
        assert!(
            fake.recorded_ctors()
                .contains(&tl_fns::stories::SendReaction::CONSTRUCTOR_ID)
        );

        // 5. GetStoryViewsList, TogglePinned, DeleteStories on own story
        let self_story =
            make_photo_story_item(101, NOW_SECS - 50, NOW_SECS + 3600, 7101, "My story", false);
        store.apply_single_tl(self_peer, &self_story, NOW_SECS);

        fake.enqueue_ok(
            tl_fns::stories::GetStoryViewsList::CONSTRUCTOR_ID,
            &tl_enums::stories::StoryViewsList::StoryViewsList(tl_types::stories::StoryViewsList {
                count: 1,
                views_count: 1,
                forwards_count: 0,
                reactions_count: 1,
                views: vec![tl_enums::StoryView::StoryView(tl_types::StoryView {
                    blocked: false,
                    blocked_my_stories_from: false,
                    user_id: bob_uid,
                    date: NOW_SECS - 10,
                    reaction: Some(tl_enums::Reaction::Emoji(tl_types::ReactionEmoji {
                        emoticon: "\u{2764}".to_owned(),
                    })),
                })],
                chats: Vec::new(),
                users: vec![tl_enums::User::User(bob_tl)],
                next_offset: None,
            }),
        );
        store.load_views_slice(self_peer, 101, String::new());
        let acts = store.take_actions();
        live.execute_actions(&mut store, acts, NOW_SECS);
        let loaded_self = store
            .lookup(FullStoryId::new(self_peer, 101))
            .expect("self story");
        assert_eq!(loaded_self.views.list.len(), 1);
        assert_eq!(loaded_self.views.list[0].peer_id, bob_peer);

        fake.enqueue_ok(tl_fns::stories::TogglePinned::CONSTRUCTOR_ID, &vec![101i32]);
        store.toggle_in_profile_list(&[FullStoryId::new(self_peer, 101)], true);
        let acts = store.take_actions();
        live.execute_actions(&mut store, acts, NOW_SECS);
        assert!(
            store
                .lookup(FullStoryId::new(self_peer, 101))
                .expect("still exists")
                .in_profile
        );

        fake.enqueue_ok(
            tl_fns::stories::DeleteStories::CONSTRUCTOR_ID,
            &vec![101i32],
        );
        store.delete_list(&[FullStoryId::new(self_peer, 101)]);
        let acts = store.take_actions();
        live.execute_actions(&mut store, acts, NOW_SECS);
        assert!(store.lookup(FullStoryId::new(self_peer, 101)).is_err());

        // 6. ActivateStealthMode
        fake.enqueue_ok(
            tl_fns::stories::ActivateStealthMode::CONSTRUCTOR_ID,
            &tl_enums::Updates::Updates(tl_types::Updates {
                updates: vec![tl_enums::Update::StoriesStealthMode(
                    tl_types::UpdateStoriesStealthMode {
                        stealth_mode: tl_enums::StoriesStealthMode::StoriesStealthMode(
                            tl_types::StoriesStealthMode {
                                active_until_date: Some(NOW_SECS + 1500),
                                cooldown_until_date: Some(NOW_SECS + 3600),
                            },
                        ),
                    },
                )],
                users: Vec::new(),
                chats: Vec::new(),
                date: NOW_SECS,
                seq: 0,
            }),
        );
        store.activate_stealth_mode();
        let acts = store.take_actions();
        live.execute_actions(&mut store, acts, NOW_SECS);
        assert!(store.stealth_mode().is_active(NOW_SECS + 10));

        // 7. Report, reply, share, export link
        fake.enqueue_ok(
            tl_fns::stories::Report::CONSTRUCTOR_ID,
            &tl_enums::ReportResult::Reported,
        );
        live.report_story(FullStoryId::new(bob_peer, 20), "Spam")
            .expect("report_story");

        fake.enqueue_ok(
            tl_fns::messages::SendMessage::CONSTRUCTOR_ID,
            &tl_enums::Updates::UpdateShortSentMessage(tl_types::UpdateShortSentMessage {
                out: true,
                id: 500,
                pts: 1,
                pts_count: 1,
                date: NOW_SECS,
                media: None,
                entities: None,
                ttl_period: None,
            }),
        );
        live.send_story_reply(FullStoryId::new(bob_peer, 20), "Nice story!", NOW_SECS)
            .expect("send_story_reply");

        fake.enqueue_ok(
            tl_fns::messages::SendMedia::CONSTRUCTOR_ID,
            &tl_enums::Updates::Updates(tl_types::Updates {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: NOW_SECS,
                seq: 0,
            }),
        );
        live.share_story(FullStoryId::new(bob_peer, 20), &[self_peer], "Look!")
            .expect("share_story");

        fake.enqueue_ok(
            tl_fns::stories::ExportStoryLink::CONSTRUCTOR_ID,
            &tl_enums::ExportedStoryLink::ExportedStoryLink(tl_types::ExportedStoryLink {
                link: "https://t.me/bob_user/s/20".to_owned(),
            }),
        );
        assert_eq!(
            live.export_story_link(FullStoryId::new(bob_peer, 20))
                .as_deref(),
            Some("https://t.me/bob_user/s/20")
        );

        // 8. Live updates stream: updateStory, updateReadStories, updateStoriesStealthMode
        fake.push_update(tl_enums::Updates::Updates(tl_types::Updates {
            updates: vec![
                tl_enums::Update::Story(tl_types::UpdateStory {
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: bob_uid }),
                    story: make_photo_story_item(
                        22,
                        NOW_SECS + 5,
                        NOW_SECS + 7200,
                        7022,
                        "Live pushed story",
                        false,
                    ),
                }),
                tl_enums::Update::ReadStories(tl_types::UpdateReadStories {
                    peer: tl_enums::Peer::User(tl_types::PeerUser { user_id: bob_uid }),
                    max_id: 22,
                }),
                tl_enums::Update::StoriesStealthMode(tl_types::UpdateStoriesStealthMode {
                    stealth_mode: tl_enums::StoriesStealthMode::StoriesStealthMode(
                        tl_types::StoriesStealthMode {
                            active_until_date: Some(NOW_SECS + 2500),
                            cooldown_until_date: Some(NOW_SECS + 7200),
                        },
                    ),
                }),
            ],
            users: Vec::new(),
            chats: Vec::new(),
            date: NOW_SECS + 5,
            seq: 1,
        }));

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut total_applied = 0;
        while Instant::now() < deadline && total_applied < 3 {
            total_applied += live.drain_story_updates(&mut store, NOW_SECS + 5);
            if total_applied < 3 {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        assert_eq!(total_applied, 3);
        assert!(store.lookup(FullStoryId::new(bob_peer, 22)).is_ok());
        assert_eq!(store.read_till(bob_peer), 22);
        assert_eq!(store.stealth_mode().enabled_till, NOW_SECS + 2500);

        // 9. Story photo download via upload.getFile + video via TelegramRangeLoader
        let photo_payload = vec![0xFFu8, 0xD8, 0xFF, 0xE0, 1, 2, 3, 4];
        let video_payload: Vec<u8> = (0..(150 * 1024usize))
            .map(|i| ((i * 19 + 3) & 0xFF) as u8)
            .collect();

        // Register a video story.
        let video_item = make_video_story_item(
            30,
            NOW_SECS,
            NOW_SECS + 3600,
            8030,
            video_payload.len() as i64,
        );
        store.apply_single_tl(self_peer, &video_item, NOW_SECS);
        live.index_story_from_store(&store, FullStoryId::new(self_peer, 30));

        let photo_for_rpc = Arc::new(photo_payload.clone());
        let video_for_rpc = Arc::new(video_payload.clone());
        fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
            if ctor != tl_fns::upload::GetFile::CONSTRUCTOR_ID {
                return None;
            }
            let req = tl_fns::upload::GetFile::from_bytes(raw).expect("decode GetFile");
            let data = match req.location {
                tl_enums::InputFileLocation::InputPhotoFileLocation(_) => &photo_for_rpc,
                tl_enums::InputFileLocation::InputDocumentFileLocation(_) => &video_for_rpc,
                _ => return None,
            };
            let start = usize::try_from(req.offset).unwrap_or(0);
            let limit = usize::try_from(req.limit).unwrap_or(0);
            let end = start.saturating_add(limit).min(data.len());
            let bytes = if start < data.len() {
                data[start..end].to_vec()
            } else {
                Vec::new()
            };
            Some(Ok(tl_enums::upload::File::File(tl_types::upload::File {
                r#type: tl_enums::storage::FileType::FilePartial,
                mtime: NOW_SECS,
                bytes,
            })
            .to_bytes()))
        }));

        // Story media loads in the background (TGB-3101): the viewer polls each frame.
        let loaded_photo = live
            .wait_story_media(FullStoryId::new(bob_peer, 20), Duration::from_secs(10))
            .expect("photo media");
        assert_eq!(loaded_photo, StoryMediaAsset::PhotoBytes(photo_payload));

        let loaded_video = live
            .wait_story_media(FullStoryId::new(self_peer, 30), Duration::from_secs(10))
            .expect("video media");
        let StoryMediaAsset::VideoStream(video_stream) = loaded_video else {
            panic!("a video story is streamed, got {loaded_video:?}");
        };
        assert_eq!(
            video_stream
                .loader()
                .load_range(0, video_payload.len())
                .expect("streamed video story"),
            video_payload
        );

        // Optional real FFmpeg StreamingPlayer over TelegramRangeLoader check if ffmpeg is present.
        let ffmpeg = tg_app::media::mock::find_ffmpeg_bin();
        if Command::new(&ffmpeg)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            let temp_mp4 = std::env::temp_dir().join(format!(
                "rustgram_live_story_video_test_{}.mp4",
                std::process::id()
            ));
            let ok = Command::new(&ffmpeg)
                .args([
                    "-loglevel",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=160x284:rate=10:duration=0.6",
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                    "-movflags",
                    "+faststart",
                ])
                .arg(&temp_mp4)
                .status()
                .is_ok_and(|s| s.success());
            if ok && let Ok(mp4_bytes) = std::fs::read(&temp_mp4) {
                let mp4_arc = Arc::new(mp4_bytes);
                let real_vid = make_video_story_item(
                    31,
                    NOW_SECS,
                    NOW_SECS + 3600,
                    8031,
                    mp4_arc.len() as i64,
                );
                store.apply_single_tl(self_peer, &real_vid, NOW_SECS);
                live.index_story_from_store(&store, FullStoryId::new(self_peer, 31));
                let mp4_for_rpc = Arc::clone(&mp4_arc);
                fake.set_dynamic_handler(Arc::new(move |ctor, raw| {
                    if ctor != tl_fns::upload::GetFile::CONSTRUCTOR_ID {
                        return None;
                    }
                    let req = tl_fns::upload::GetFile::from_bytes(raw).expect("decode GetFile");
                    let start = usize::try_from(req.offset).unwrap_or(0);
                    let limit = usize::try_from(req.limit).unwrap_or(0);
                    let end = start.saturating_add(limit).min(mp4_for_rpc.len());
                    let bytes = if start < mp4_for_rpc.len() {
                        mp4_for_rpc[start..end].to_vec()
                    } else {
                        Vec::new()
                    };
                    Some(Ok(tl_enums::upload::File::File(tl_types::upload::File {
                        r#type: tl_enums::storage::FileType::FileMp4,
                        mtime: NOW_SECS,
                        bytes,
                    })
                    .to_bytes()))
                }));
                let player = live
                    .open_story_video_player(
                        FullStoryId::new(self_peer, 31),
                        PlaybackOptions::default(),
                    )
                    .expect("open StreamingPlayer over TelegramRangeLoader");
                assert!(player.info().video.is_some());
            }
            let _ = std::fs::remove_file(&temp_mp4);
        }

        let _ = self_uid;
        let ctors = fake.recorded_ctors();
        assert!(ctors.contains(&tl_fns::stories::GetPeerStories::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::stories::GetStoriesByID::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::stories::IncrementStoryViews::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::stories::GetPeerMaxIDs::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::stories::SendReaction::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::stories::GetStoryViewsList::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::stories::TogglePinned::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::stories::DeleteStories::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::stories::ActivateStealthMode::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::stories::Report::CONSTRUCTOR_ID));
        assert!(ctors.contains(&tl_fns::upload::GetFile::CONSTRUCTOR_ID));

        shutdown_harness(live, runtime);
    }
}

mod suggestions_tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use tg_api::{DcId, InvocationError, RpcClient};
    use tg_tl::{Deserializable, RemoteCall, Serializable, enums as tl_enums, types as tl_types};
    use tokio::sync::broadcast;

    use tg_app::dialogs::suggestions::PromoSuggestionsState;
    use tg_app::live::suggestions::*;

    type RpcResponseQueue = Arc<Mutex<VecDeque<Result<Vec<u8>, InvocationError>>>>;

    #[derive(Clone)]
    struct FakePromoRpc {
        responses: RpcResponseQueue,
        updates_tx: broadcast::Sender<tl_enums::Updates>,
    }

    impl FakePromoRpc {
        fn new() -> Self {
            let (updates_tx, _) = broadcast::channel(8);
            Self {
                responses: Arc::new(Mutex::new(VecDeque::new())),
                updates_tx,
            }
        }

        fn push_ok<T: Serializable>(&self, resp: &T) {
            self.responses
                .lock()
                .expect("lock")
                .push_back(Ok(resp.to_bytes()));
        }
    }

    impl RpcClient for FakePromoRpc {
        type Error = InvocationError;

        async fn invoke<R: RemoteCall + Send + Sync>(
            &self,
            _req: &R,
        ) -> Result<R::Return, Self::Error> {
            let next = self
                .responses
                .lock()
                .expect("lock")
                .pop_front()
                .ok_or(InvocationError::Dropped)??;
            R::Return::from_bytes(&next).map_err(|e| InvocationError::Deserialize(e.to_string()))
        }

        async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
            &self,
            _dc_id: DcId,
            req: &R,
        ) -> Result<R::Return, Self::Error> {
            self.invoke(req).await
        }

        fn main_dc_id(&self) -> DcId {
            2
        }

        fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
            Some(self.updates_tx.subscribe())
        }
    }

    fn make_tl_text(text: &str) -> tl_enums::TextWithEntities {
        tl_enums::TextWithEntities::TextWithEntities(tl_types::TextWithEntities {
            text: text.to_owned(),
            entities: Vec::new(),
        })
    }

    #[tokio::test]
    async fn live_fetch_and_dismiss_promo_data_over_fake_rpc() {
        let client = FakePromoRpc::new();

        let promo_tl = tl_enums::help::PromoData::PromoData(tl_types::help::PromoData {
            proxy: false,
            expires: 1_950_000_000,
            peer: Some(tl_enums::Peer::Channel(tl_types::PeerChannel {
                channel_id: 777,
            })),
            psa_type: Some("info".to_owned()),
            psa_message: Some("Welcome".to_owned()),
            pending_suggestions: vec!["BIRTHDAY_SETUP".to_owned()],
            dismissed_suggestions: vec![],
            custom_pending_suggestion: Some(tl_enums::PendingSuggestion::PendingSuggestion(
                tl_types::PendingSuggestion {
                    suggestion: "CUSTOM_NEWS".to_owned(),
                    title: make_tl_text("Release Notes"),
                    description: make_tl_text("See what is new"),
                    url: "https://t.me/telegram".to_owned(),
                },
            )),
            chats: vec![],
            users: vec![],
        });
        client.push_ok(&promo_tl);
        client.push_ok(&true);

        let snap = fetch_promo_data(&client).await.expect("getPromoData");
        assert_eq!(snap.expires, 1_950_000_000);
        assert_eq!(snap.pending_suggestions, vec!["BIRTHDAY_SETUP"]);
        assert_eq!(
            snap.custom_suggestion.as_ref().unwrap().title,
            "Release Notes"
        );

        let mut state = PromoSuggestionsState::new();
        state.apply_snapshot(snap);
        assert_eq!(state.active().unwrap().key, "BIRTHDAY_SETUP");

        let ok = dismiss_promo_suggestion(&client, empty_input_peer(), "BIRTHDAY_SETUP")
            .await
            .expect("dismissSuggestion");
        assert!(ok);
        state.mark_dismissed("BIRTHDAY_SETUP");
        assert_eq!(state.active().unwrap().key, "CUSTOM_NEWS");
    }
}
