//! Byte-level object-safe RPC client trait ([`RawRpcClient`]) and type-erased wrapper
//! ([`ErasedRpcClient`]) implementing `tg_api::RpcClient`.
//!
//! Used by `LiveSession` and `SessionRuntime` so all live backend code compiles and
//! monomorphizes once instead of once per concrete `RpcClient` / test fake.

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tg_api::rpc::{DcId, InvocationError, RpcClient};
use tg_tl::{Deserializable, RawObject, RemoteCall, Serializable, enums as tl_enums};
use tokio::sync::broadcast;

struct SerializedRawCall {
    bytes: Vec<u8>,
}

impl Serializable for SerializedRawCall {
    fn serialize(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&self.bytes);
    }
}

impl RemoteCall for SerializedRawCall {
    type Return = RawObject;
}

/// Byte-level object-safe RPC client trait used to type-erase `tg_api::RpcClient`.
pub trait RawRpcClient: Send + Sync + Any {
    /// Returns `&dyn Any` for optional downcasting (e.g. `MtprotoRpc` proxy monitoring).
    fn as_any(&self) -> &dyn Any;

    /// Invokes a serialized TL request on the main DC (`None`) or a specific DC (`Some(dc_id)`).
    fn invoke_raw(
        &self,
        dc_id: Option<DcId>,
        bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, InvocationError>> + Send + '_>>;

    /// Invokes a serialized TL request with `handle_flood_errors` enabled.
    fn invoke_raw_handle_flood(
        &self,
        dc_id: Option<DcId>,
        bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, InvocationError>> + Send + '_>>;

    /// Returns the currently selected main DC ID.
    fn main_dc_id(&self) -> DcId;

    /// Switches the main DC ID.
    fn set_main_dc_id(&self, dc_id: DcId);

    /// Subscribes to the live `tg_tl::enums::Updates` push stream, if supported.
    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>>;

    /// Sends a keepalive/liveness ping to the main DC.
    fn ping_raw(&self) -> Pin<Box<dyn Future<Output = Result<(), InvocationError>> + Send + '_>>;

    /// Restarts active transport sessions.
    fn restart_sessions(&self);
}

impl<C: RpcClient + Send + Sync + 'static> RawRpcClient for C {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn invoke_raw(
        &self,
        dc_id: Option<DcId>,
        bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, InvocationError>> + Send + '_>> {
        Box::pin(async move {
            let call = SerializedRawCall { bytes };
            let res = match dc_id {
                Some(dc) => self.invoke_in_dc(dc, &call).await,
                None => self.invoke(&call).await,
            };
            res.map(|raw| raw.0).map_err(Into::into)
        })
    }

    fn invoke_raw_handle_flood(
        &self,
        dc_id: Option<DcId>,
        bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, InvocationError>> + Send + '_>> {
        Box::pin(async move {
            let call = SerializedRawCall { bytes };
            let res = match dc_id {
                Some(dc) => self.invoke_in_dc_handle_flood(dc, &call).await,
                None => self.invoke_handle_flood(&call).await,
            };
            res.map(|raw| raw.0).map_err(Into::into)
        })
    }

    fn main_dc_id(&self) -> DcId {
        RpcClient::main_dc_id(self)
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        RpcClient::set_main_dc_id(self, dc_id);
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        RpcClient::subscribe_updates(self)
    }

    fn ping_raw(&self) -> Pin<Box<dyn Future<Output = Result<(), InvocationError>> + Send + '_>> {
        Box::pin(async move { RpcClient::ping(self).await.map_err(Into::into) })
    }

    fn restart_sessions(&self) {
        RpcClient::restart_sessions(self);
    }
}

/// Type-erased [`RpcClient`] backed by `Arc<dyn RawRpcClient>`.
#[derive(Clone)]
pub struct ErasedRpcClient(Arc<dyn RawRpcClient>);

impl ErasedRpcClient {
    /// Wraps any concrete [`RpcClient`] into an [`ErasedRpcClient`] (no-op if already erased).
    #[must_use]
    pub fn new<C: RpcClient + Send + Sync + 'static>(client: C) -> Self {
        if let Some(already) = (&client as &dyn Any).downcast_ref::<Self>() {
            return already.clone();
        }
        Self(Arc::new(client))
    }

    /// Downcasts the inner concrete client to `&T`, if it matches.
    #[must_use]
    pub fn downcast_ref<T: 'static>(&self) -> Option<&T> {
        self.0.as_any().downcast_ref::<T>()
    }
}

impl RpcClient for ErasedRpcClient {
    type Error = InvocationError;

    async fn invoke<R: RemoteCall + Send + Sync>(&self, req: &R) -> Result<R::Return, Self::Error> {
        let raw = self.0.invoke_raw(None, req.to_bytes()).await?;
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    async fn invoke_in_dc<R: RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let raw = self.0.invoke_raw(Some(dc_id), req.to_bytes()).await?;
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    async fn invoke_handle_flood<R: RemoteCall + Send + Sync>(
        &self,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let raw = self.0.invoke_raw_handle_flood(None, req.to_bytes()).await?;
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    async fn invoke_in_dc_handle_flood<R: RemoteCall + Send + Sync>(
        &self,
        dc_id: DcId,
        req: &R,
    ) -> Result<R::Return, Self::Error> {
        let raw = self
            .0
            .invoke_raw_handle_flood(Some(dc_id), req.to_bytes())
            .await?;
        R::Return::from_bytes(&raw).map_err(|e| InvocationError::Deserialize(e.to_string()))
    }

    fn main_dc_id(&self) -> DcId {
        self.0.main_dc_id()
    }

    fn set_main_dc_id(&self, dc_id: DcId) {
        self.0.set_main_dc_id(dc_id);
    }

    fn subscribe_updates(&self) -> Option<broadcast::Receiver<tl_enums::Updates>> {
        self.0.subscribe_updates()
    }

    async fn ping(&self) -> Result<(), Self::Error> {
        self.0.ping_raw().await
    }

    fn restart_sessions(&self) {
        self.0.restart_sessions();
    }
}
