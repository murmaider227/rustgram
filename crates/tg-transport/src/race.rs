//! Concurrent connection racing across transports and IP addresses.
//!
//! Port of `SessionPrivate::_testConnections`, `appendTestConnection`, `onConnected`,
//! and `waitBetterFailed` in `mtproto/session_private.cpp`.

use std::collections::HashMap;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until};

use crate::connection::{Connection, ConnectionConfig, FULL_CONNECTION_TIMEOUT};
use crate::error::Error;
use crate::http_transport::{HttpConnection, HttpConnectionConfig};

/// Grace period to wait for a higher-priority connection after a lower-priority candidate
/// connects (`kWaitForBetterTimeout` in `mtproto/session_private.cpp`).
pub const WAIT_FOR_BETTER_TIMEOUT: Duration = Duration::from_secs(2);

/// Compute candidate priority matching `SessionPrivate::appendTestConnection`
/// (`mtproto/session_private.cpp:214-216`).
#[must_use]
pub const fn compute_priority(
    is_ipv6: bool,
    prefer_ipv6: bool,
    is_tcp: bool,
    has_secret: bool,
) -> i32 {
    let ip_score = if is_ipv6 {
        if prefer_ipv6 { 2 } else { 0 }
    } else {
        1
    };
    let proto_score = if is_tcp { 1 } else { 0 };
    let secret_score = if has_secret { 1 } else { 0 };
    ip_score + proto_score + secret_score
}

/// Transport endpoint variant for a [`RaceCandidate`].
#[derive(Debug, Clone)]
pub enum RaceEndpoint {
    /// TCP connection (direct, SOCKS5, HTTP CONNECT, or MTProxy).
    Tcp {
        /// Target IP address or hostname.
        address: String,
        /// Target TCP port.
        port: u16,
        /// TCP connection settings.
        config: ConnectionConfig,
    },
    /// Plain HTTP POST `/api` connection.
    Http {
        /// Target IP address or hostname.
        address: String,
        /// HTTP connection settings.
        config: HttpConnectionConfig,
    },
}

/// Single connection candidate participating in [`race_connections`].
#[derive(Debug, Clone)]
pub struct RaceCandidate {
    /// Endpoint to connect to.
    pub endpoint: RaceEndpoint,
    /// Relative priority (higher is preferred; see [`compute_priority`]).
    pub priority: i32,
    /// Whether to run a `req_pq_multi` / `resPQ` connectivity check before declaring success.
    pub probe_pq: bool,
}

impl RaceCandidate {
    /// Create a TCP race candidate with automatically computed tdesktop priority.
    #[must_use]
    pub fn tcp(
        address: impl Into<String>,
        port: u16,
        config: ConnectionConfig,
        prefer_ipv6: bool,
        probe_pq: bool,
    ) -> Self {
        let address = address.into();
        let is_ipv6 = address.contains(':');
        let has_secret = !config.protocol_secret.is_empty();
        let priority = compute_priority(is_ipv6, prefer_ipv6, true, has_secret);
        Self {
            endpoint: RaceEndpoint::Tcp {
                address,
                port,
                config,
            },
            priority,
            probe_pq,
        }
    }

    /// Create an HTTP race candidate with automatically computed tdesktop priority.
    #[must_use]
    pub fn http(
        address: impl Into<String>,
        config: HttpConnectionConfig,
        prefer_ipv6: bool,
        probe_pq: bool,
    ) -> Self {
        let address = address.into();
        let is_ipv6 = address.contains(':');
        let priority = compute_priority(is_ipv6, prefer_ipv6, false, false);
        Self {
            endpoint: RaceEndpoint::Http { address, config },
            priority,
            probe_pq,
        }
    }
}

/// Tuning parameters for [`race_connections`].
#[derive(Debug, Clone, Copy)]
pub struct RaceOptions {
    /// Maximum time to wait for a higher-priority candidate once a lower-priority one succeeds
    /// (`kWaitForBetterTimeout` = 2s).
    pub wait_for_better: Duration,
    /// Overall timeout for the entire race if no candidate connects.
    pub total_timeout: Duration,
    /// Timeout for each candidate's `probe_pq` step when `probe_pq` is enabled.
    pub probe_timeout: Duration,
}

impl Default for RaceOptions {
    fn default() -> Self {
        Self {
            wait_for_better: WAIT_FOR_BETTER_TIMEOUT,
            total_timeout: FULL_CONNECTION_TIMEOUT,
            probe_timeout: FULL_CONNECTION_TIMEOUT,
        }
    }
}

/// Connected transport returned by [`race_connections`].
pub enum RacedTransport {
    /// Connected TCP MTProto transport.
    Tcp(Connection),
    /// Connected plain HTTP MTProto transport.
    Http(HttpConnection),
}

/// Winning result from [`race_connections`].
pub struct RacedConnection {
    /// Index of the winning candidate in the input slice.
    pub candidate_index: usize,
    /// Priority of the winning candidate.
    pub priority: i32,
    /// The established transport.
    pub transport: RacedTransport,
}

/// Race multiple connection candidates concurrently and pick the first highest-priority winner.
///
/// Mirrors `SessionPrivate::onConnected` + `waitBetterFailed`:
/// - If a candidate connects and no still-pending candidate has strictly higher `priority`,
///   it is returned immediately and all remaining attempts are aborted.
/// - If a higher-priority candidate is still pending, we keep the connected winner and wait
///   up to `options.wait_for_better` for a better candidate to finish.
/// - If all higher-priority candidates fail or the grace timer expires, we return the best
///   connected candidate.
pub async fn race_connections(
    candidates: Vec<RaceCandidate>,
    options: RaceOptions,
) -> Result<RacedConnection, Error> {
    if candidates.is_empty() {
        return Err(Error::AllCandidatesFailed("empty candidate list".into()));
    }

    let mut pending_priorities: HashMap<usize, i32> = HashMap::with_capacity(candidates.len());
    let mut set: JoinSet<(usize, i32, Result<RacedTransport, Error>)> = JoinSet::new();

    for (idx, candidate) in candidates.into_iter().enumerate() {
        let prio = candidate.priority;
        let probe_pq = candidate.probe_pq;
        let probe_timeout = options.probe_timeout;
        pending_priorities.insert(idx, prio);

        set.spawn(async move {
            let res = match candidate.endpoint {
                RaceEndpoint::Tcp {
                    address,
                    port,
                    config,
                } => match Connection::connect(&address, port, config).await {
                    Ok(mut conn) => {
                        if probe_pq {
                            if let Err(e) = conn.probe_pq(probe_timeout).await {
                                Err(e)
                            } else {
                                Ok(RacedTransport::Tcp(conn))
                            }
                        } else {
                            Ok(RacedTransport::Tcp(conn))
                        }
                    }
                    Err(e) => Err(e),
                },
                RaceEndpoint::Http { address, config } => {
                    let mut conn = HttpConnection::new(address, config);
                    if probe_pq {
                        match conn.probe_pq().await {
                            Ok(_) => Ok(RacedTransport::Http(conn)),
                            Err(e) => Err(e),
                        }
                    } else {
                        Ok(RacedTransport::Http(conn))
                    }
                }
            };
            (idx, prio, res)
        });
    }

    let overall_deadline = Instant::now() + options.total_timeout;
    let mut better_deadline: Option<Instant> = None;
    let mut best: Option<RacedConnection> = None;
    let mut last_error: Option<Error> = None;

    loop {
        if pending_priorities.is_empty() {
            break;
        }
        let next_deadline = match better_deadline {
            Some(d) => d.min(overall_deadline),
            None => overall_deadline,
        };

        let joined = tokio::select! {
            res = set.join_next() => res,
            () = sleep_until(next_deadline) => {
                if let Some(winner) = best.take() {
                    set.abort_all();
                    return Ok(winner);
                }
                set.abort_all();
                return Err(last_error.unwrap_or(Error::Timeout));
            }
        };

        let Some(join_result) = joined else {
            break;
        };

        match join_result {
            Ok((idx, prio, Ok(transport))) => {
                pending_priorities.remove(&idx);
                let is_better = best.as_ref().is_none_or(|b| prio > b.priority);
                if is_better {
                    best = Some(RacedConnection {
                        candidate_index: idx,
                        priority: prio,
                        transport,
                    });
                }
                if let Some(current_best) = &best {
                    let any_higher_pending = pending_priorities
                        .values()
                        .any(|&p| p > current_best.priority);
                    if !any_higher_pending {
                        set.abort_all();
                        return Ok(best.expect("best is Some"));
                    }
                    if better_deadline.is_none() {
                        better_deadline = Some(Instant::now() + options.wait_for_better);
                    }
                }
            }
            Ok((idx, _prio, Err(e))) => {
                pending_priorities.remove(&idx);
                last_error = Some(e);
                if let Some(current_best) = &best {
                    let any_higher_pending = pending_priorities
                        .values()
                        .any(|&p| p > current_best.priority);
                    if !any_higher_pending {
                        set.abort_all();
                        return Ok(best.expect("best is Some"));
                    }
                }
            }
            Err(join_err) => {
                last_error = Some(Error::AllCandidatesFailed(join_err.to_string()));
            }
        }
    }

    set.abort_all();
    if let Some(winner) = best {
        Ok(winner)
    } else {
        Err(last_error
            .unwrap_or_else(|| Error::AllCandidatesFailed("all candidates failed".into())))
    }
}
