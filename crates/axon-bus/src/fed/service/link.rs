//! The view of the service that frame handlers keep.

use std::sync::{Arc, Weak};

use iroh::EndpointAddr;

use super::{Handle, Shared, Stop};

/// A non-owning `Handle` for frame handlers: a `Handle` stored in the service's own handler
/// table would keep the service alive. Its calls do nothing once the service is gone.
#[derive(Clone)]
pub struct Link(pub(super) Weak<Shared>);

impl Link {
    pub fn reload(&self) {
        self.0
            .upgrade()
            .inspect(|shared| shared.reload.notify_one());
    }

    pub fn add_addr(&self, addr: EndpointAddr) {
        self.0.upgrade().inspect(|shared| shared.add_addr(addr));
    }
}

/// A `Handle` that does not keep the service alive, for frame handlers that sometimes need
/// the whole handle (to send a request): they `upgrade` it per call.
#[derive(Clone)]
pub struct WeakHandle {
    shared: Weak<Shared>,
    stop: Weak<Stop>,
    relay_setting: Arc<str>,
}

impl WeakHandle {
    pub fn upgrade(&self) -> Option<Handle> {
        Some(Handle {
            shared: self.shared.upgrade()?,
            stop: self.stop.upgrade()?,
            relay_setting: self.relay_setting.clone(),
        })
    }
}

impl Handle {
    pub fn downgrade(&self) -> WeakHandle {
        WeakHandle {
            shared: Arc::downgrade(&self.shared),
            stop: Arc::downgrade(&self.stop),
            relay_setting: self.relay_setting.clone(),
        }
    }
}
