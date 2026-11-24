//! The mock push service: the controller's Web Push sender, replaced by one that records each
//! payload as the push service would receive it (before encryption).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use controlroom_controller::store::PushSubscriptionRow;
use controlroom_controller::test_support::wait_until;
use controlroom_controller::{PushConfig, PushOutcome, PushSender};
use serde_json::Value;

/// Records every push and accepts it.
#[derive(Default)]
pub struct MockPush {
    payloads: Mutex<Vec<Value>>,
}

impl MockPush {
    /// The controller's push configuration delivering to `self`.
    pub fn config(self: &Arc<Self>) -> PushConfig {
        PushConfig {
            vapid_public_key: "BTestVapidPublicKey".into(),
            sender: self.clone(),
        }
    }

    /// Every payload so far, in delivery order.
    pub fn payloads(&self) -> Vec<Value> {
        self.payloads.lock().unwrap().clone()
    }

    /// Waits until a push about session `id` arrived; returns the first.
    pub async fn wait_for(&self, id: &str) -> Value {
        let about = |p: &Value| p["session_id"] == id;
        wait_until(&format!("a push about {id}"), || {
            self.payloads().iter().any(about)
        })
        .await;
        self.payloads().into_iter().find(about).unwrap()
    }
}

impl PushSender for MockPush {
    fn send<'a>(
        &'a self,
        _sub: &'a PushSubscriptionRow,
        payload: &'a [u8],
        _urgent: bool,
    ) -> Pin<Box<dyn Future<Output = PushOutcome> + Send + 'a>> {
        let payload: Value = serde_json::from_slice(payload).unwrap();
        self.payloads.lock().unwrap().push(payload);
        Box::pin(async { PushOutcome::Delivered })
    }
}
