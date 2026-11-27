//! Independent requests a screen needs together go out at once, so its first frame waits for one
//! round trip instead of one per request (tdesktop sends them all and fills each row when its
//! answer lands).

use std::time::Duration;

use tg_api::RpcClient;
use tokio::task::JoinHandle;

use crate::ErasedRpcClient;
use crate::media::with_rpc_timeout;

/// Starts `req` on the current runtime with `timeout`; call inside the runtime (e.g. in a
/// `block_on_runtime` future) and await the handle with [`joined`].
pub(crate) fn spawn_rpc<R>(
    rpc: &ErasedRpcClient,
    req: R,
    timeout: Duration,
) -> JoinHandle<Result<R::Return, String>>
where
    R: tg_tl::RemoteCall + Send + Sync + 'static,
    R::Return: Send + 'static,
{
    let rpc = rpc.clone();
    tokio::spawn(
        async move { with_rpc_timeout(timeout, async move { rpc.invoke(&req).await }).await },
    )
}

/// The answer of a [`spawn_rpc`] request; a request task that panicked is an error.
pub(crate) async fn joined<T>(request: JoinHandle<Result<T, String>>) -> Result<T, String> {
    request.await.map_err(|err| err.to_string())?
}
