//! Live paid-message fee revenue, fee-exception toggling, and contact-requirements RPCs
//! (`account.getPaidMessagesRevenue`, `account.toggleNoPaidMessagesException`,
//! `users.getRequirementsToContact`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/window/window_peer_menu.cpp` (`PeerMenuConfirmToggleFee`)
//! - `Telegram/SourceFiles/api/api_premium.cpp` (`Premium::requestPremiumRequiredSlice`)

use std::sync::Arc;
use std::time::Duration;

use tg_api::RpcClient;
use tg_api::convert::{input_peer_for, input_user_for};
use tg_data::{Session, UserFlags, UserId as DataUserId};
use tg_tl::{RemoteCall, enums as tl_enums, functions as tl_fns};

use crate::convert::to_data_peer;
use crate::media::{block_on_runtime, with_rpc_timeout};
use crate::session::LiveSession;
use tg_app_core::info::backend::{
    PaidMessagesRevenue, RequirementToContact, UserContactRequirement,
};
use tg_app_core::stand_in::types::PeerId;

/// Default timeout for paid-message & contact-requirement RPC calls.
const PAID_MESSAGES_RPC_TIMEOUT: Duration = Duration::from_secs(12);

/// Maximum users per `users.getRequirementsToContact` batch (`kPerRequest = 100` in
/// `Telegram/SourceFiles/api/api_premium.cpp:423`).
pub const REQUIREMENTS_TO_CONTACT_BATCH_LIMIT: usize = 100;

fn invoke_paid_rpc<R>(live: &LiveSession, req: R) -> Result<R::Return, String>
where
    R: RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = live.runtime().rpc().clone();
    block_on_runtime(live.runtime().tokio_handle(), async move {
        with_rpc_timeout(
            PAID_MESSAGES_RPC_TIMEOUT,
            async move { rpc.invoke(&req).await },
        )
        .await
    })
}

fn read_session_sync<R: Send>(live: &LiveSession, f: impl FnOnce(&Session) -> R + Send) -> R {
    let lock = live.runtime().session();
    if let Ok(guard) = lock.try_read() {
        return f(&guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let guard = arc.read().await;
        f(&guard)
    })
}

fn write_session_sync<R: Send>(live: &LiveSession, f: impl FnOnce(&mut Session) -> R + Send) -> R {
    let lock = live.runtime().session();
    if let Ok(mut guard) = lock.try_write() {
        return f(&mut guard);
    }
    let arc = Arc::clone(lock);
    block_on_runtime(live.runtime().tokio_handle(), async move {
        let mut guard = arc.write().await;
        f(&mut guard)
    })
}

fn resolve_input_user(
    live: &LiveSession,
    user_peer: PeerId,
) -> Result<tl_enums::InputUser, String> {
    let data_peer = to_data_peer(user_peer);
    let Some(uid) = data_peer.to_user() else {
        return Err("Peer is not a user.".to_owned());
    };
    let input = read_session_sync(live, |s| input_user_for(s, uid));
    if matches!(input, tl_enums::InputUser::Empty) {
        return Err("User not found in session.".to_owned());
    }
    Ok(input)
}

fn resolve_optional_parent_peer(
    live: &LiveSession,
    parent_peer: Option<PeerId>,
) -> Result<Option<tl_enums::InputPeer>, String> {
    let Some(pid) = parent_peer else {
        return Ok(None);
    };
    let data_peer = to_data_peer(pid);
    let input = read_session_sync(live, |s| input_peer_for(s, data_peer));
    if matches!(input, tl_enums::InputPeer::Empty) {
        return Err("Parent peer not found in session.".to_owned());
    }
    Ok(Some(input))
}

/// Fetches paid-message Stars revenue charged to `user_id` (optionally scoped to a channel
/// direct-messages `parent_peer`) via `account.getPaidMessagesRevenue`
/// (`Telegram/SourceFiles/window/window_peer_menu.cpp:4803-4810`).
pub fn get_paid_messages_revenue_live(
    live: &LiveSession,
    parent_peer: Option<PeerId>,
    user_id: PeerId,
) -> Result<PaidMessagesRevenue, String> {
    let tl_parent = resolve_optional_parent_peer(live, parent_peer)?;
    let tl_user = resolve_input_user(live, user_id)?;
    let tl_enums::account::PaidMessagesRevenue::PaidMessagesRevenue(res) = invoke_paid_rpc(
        live,
        tl_fns::account::GetPaidMessagesRevenue {
            parent_peer: tl_parent,
            user_id: tl_user,
        },
    )?;
    Ok(PaidMessagesRevenue {
        stars_amount: res.stars_amount,
    })
}

/// Adds or removes a paid-messages fee exception for `user_id` via
/// `account.toggleNoPaidMessagesException`
/// (`Telegram/SourceFiles/window/window_peer_menu.cpp:4744-4761`).
///
/// - `require_payment == false`: removes the Stars fee for `user_id` (and refunds previously
///   charged Stars when `refund_charged == true`).
/// - `require_payment == true`: re-enables the paid-message requirement for `user_id`.
pub fn toggle_no_paid_messages_exception_live(
    live: &LiveSession,
    parent_peer: Option<PeerId>,
    user_id: PeerId,
    refund_charged: bool,
    require_payment: bool,
) -> Result<bool, String> {
    let tl_parent = resolve_optional_parent_peer(live, parent_peer)?;
    let tl_user = resolve_input_user(live, user_id)?;
    let ok = invoke_paid_rpc(
        live,
        tl_fns::account::ToggleNoPaidMessagesException {
            refund_charged,
            require_payment,
            parent_peer: tl_parent,
            user_id: tl_user,
        },
    )?;
    // Mirror `window_peer_menu.cpp:4754-4755`: clear cached per-message Stars on 1:1 DM fee removal.
    if ok
        && parent_peer.is_none()
        && !require_payment
        && let Some(uid) = to_data_peer(user_id).to_user()
    {
        write_session_sync(live, |s| {
            if let Some(user) = s.user_mut(uid) {
                user.stars_per_message = 0;
                user.flags.remove(UserFlags::HAS_STARS_PER_MESSAGE);
            }
        });
        live.bridge_state().notify_ui();
    }
    Ok(ok)
}

/// Fetches contact restrictions (none / Premium required / Stars per message) for `user_ids`
/// via `users.getRequirementsToContact`, batching in slices of 100 and updating cached
/// [`tg_data::User`] flags (`Telegram/SourceFiles/api/api_premium.cpp:417-485`).
pub fn get_requirements_to_contact_live(
    live: &LiveSession,
    user_ids: &[PeerId],
) -> Result<Vec<UserContactRequirement>, String> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut resolved: Vec<(PeerId, DataUserId, tl_enums::InputUser)> =
        Vec::with_capacity(user_ids.len());
    for &pid in user_ids {
        let data_peer = to_data_peer(pid);
        let Some(uid) = data_peer.to_user() else {
            return Err("users.getRequirementsToContact requires user peer IDs.".to_owned());
        };
        let input = resolve_input_user(live, pid)?;
        resolved.push((pid, uid, input));
    }

    let mut out = Vec::with_capacity(resolved.len());
    for chunk in resolved.chunks(REQUIREMENTS_TO_CONTACT_BATCH_LIMIT) {
        let id_vec: Vec<tl_enums::InputUser> = chunk.iter().map(|(_, _, u)| u.clone()).collect();
        let tl_list =
            invoke_paid_rpc(live, tl_fns::users::GetRequirementsToContact { id: id_vec })?;

        let mut updates_to_apply: Vec<(DataUserId, bool, i64)> = Vec::with_capacity(chunk.len());
        for (idx, &(pid, uid, _)) in chunk.iter().enumerate() {
            // Mirror `api_premium.cpp:457-467`: missing trailing elements default to Empty.
            let req = match tl_list.get(idx) {
                Some(tl_enums::RequirementToContact::Premium) => {
                    updates_to_apply.push((uid, true, 0));
                    RequirementToContact::Premium
                }
                Some(tl_enums::RequirementToContact::PaidMessages(d)) => {
                    updates_to_apply.push((uid, false, d.stars_amount));
                    RequirementToContact::PaidMessages {
                        stars_amount: d.stars_amount,
                    }
                }
                Some(tl_enums::RequirementToContact::Empty) | None => {
                    updates_to_apply.push((uid, false, 0));
                    RequirementToContact::Empty
                }
            };
            out.push(UserContactRequirement {
                user_id: pid,
                requirement: req,
            });
        }

        // Mirror `api_premium.cpp:446-455` user flag & stars_per_message update.
        write_session_sync(live, |s| {
            for (uid, require_premium, stars) in updates_to_apply {
                if let Some(user) = s.user_mut(uid) {
                    user.stars_per_message = stars.clamp(0, i64::from(i32::MAX)) as i32;
                    user.flags.remove(UserFlags::REQUIRES_PREMIUM_TO_WRITE);
                    user.flags
                        .insert(UserFlags::MESSAGE_MONEY_RESTRICTIONS_KNOWN);
                    if require_premium {
                        user.flags.insert(
                            UserFlags::REQUIRES_PREMIUM_TO_WRITE
                                | UserFlags::HAS_REQUIRE_PREMIUM_TO_WRITE,
                        );
                    }
                    if stars > 0 {
                        user.flags.insert(UserFlags::HAS_STARS_PER_MESSAGE);
                    }
                }
            }
        });
    }

    live.bridge_state().notify_ui();
    Ok(out)
}
