//! The typed layer over the untyped call core.

use serde::Serialize;
use serde::de::DeserializeOwned;

/// A daemon API method: its wire name and its params and result types.
///
/// The protocol crate defines the methods; this trait is the seam
/// [`super::DaemonClient::call`] needs from them.
pub trait RpcMethod {
    /// The JSON-RPC method name.
    const NAME: &'static str;
    /// The request params.
    type Params: Serialize;
    /// The success result.
    type Result: DeserializeOwned;
}
