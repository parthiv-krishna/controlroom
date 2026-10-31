//! The controller's side of every execution machine: SSH, keys, bootstrap and
//! the daemon client.
//!
//! - [`keys`]: generate and import SSH keys, public key lines, fingerprints.
//! - [`master_key`] and [`seal`]: encrypt private keys at rest under the master key.
//! - [`ssh`]: russh connections with host-key pinning and multiplexed channels.
//! - [`bootstrap`]: probe, install, upgrade and roll back `controlroomd`;
//!   `authorized_keys` edits and key rotation.
//! - [`daemon`]: the daemon's line-delimited JSON-RPC client over any byte stream.
//! - [`shell`]: quoting for commands sent over SSH exec.

pub mod bootstrap;
pub mod daemon;
pub mod keys;
pub mod master_key;
pub mod seal;
pub mod shell;
pub mod ssh;
#[cfg(all(unix, feature = "test-support"))]
pub mod test_support;

#[cfg(test)]
#[path = "../tests/support/test_keys.rs"]
mod test_keys;

pub use keys::{KeyError, SshPrivateKey};
pub use master_key::{MasterKey, MasterKeyError};
pub use seal::SealError;
