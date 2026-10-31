//! Rotating the key a machine accepts.

use super::BootstrapError;
use super::authorized_keys::{add_authorized_key, remove_authorized_key};
use crate::keys::SshPrivateKey;
use crate::ssh::{ConnectOptions, SshConnection, SshTarget, connect};

/// Moves a machine from its current key to `new_key`:
///
/// 1. append `new_key` to `authorized_keys` over `conn` (authenticated with the old key);
/// 2. connect with `new_key` to prove it works;
/// 3. remove `old_key_line`'s key over the new connection.
///
/// Returns the new connection. If step 2 fails, the new key is removed again
/// (best effort) and the old key keeps working.
pub async fn rotate_key(
    conn: &SshConnection,
    target: &SshTarget,
    new_key: &SshPrivateKey,
    old_key_line: &str,
    options: &ConnectOptions,
) -> Result<SshConnection, BootstrapError> {
    let new_line = new_key.public_key_line();
    let added = add_authorized_key(conn, &new_line).await?;
    let new_conn = match connect(target, new_key, Some(conn.host_key()), options).await {
        Ok(new_conn) => new_conn,
        Err(e) => {
            if added && let Err(undo) = remove_authorized_key(conn, &new_line).await {
                tracing::warn!("could not remove the new key after a failed rotation: {undo}");
            }
            return Err(e.into());
        }
    };
    remove_authorized_key(&new_conn, old_key_line).await?;
    Ok(new_conn)
}
