//! The view of the service that frame handlers keep.

use std::sync::Weak;

use iroh::EndpointAddr;

use super::Shared;

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
