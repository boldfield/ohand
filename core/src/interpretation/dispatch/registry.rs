use crate::providers::contracts::{ProviderAdapter, ProviderProtocol};

/// The protocol adapters this build can talk to, one per [`ProviderProtocol`].
///
/// Registering an adapter confers no permission: a job still needs a stored profile pin, a route
/// and a capability grant before the dispatcher will use the adapter selected here.
#[derive(Default)]
pub struct AdapterRegistry {
    adapters: Vec<(ProviderProtocol, Box<dyn ProviderAdapter>)>,
}

impl AdapterRegistry {
    pub fn new() -> AdapterRegistry {
        AdapterRegistry::default()
    }

    /// Register `adapter` for `protocol`, replacing any adapter registered earlier.
    pub fn with_adapter(
        mut self,
        protocol: ProviderProtocol,
        adapter: impl ProviderAdapter + 'static,
    ) -> AdapterRegistry {
        self.adapters.retain(|(existing, _)| *existing != protocol);
        self.adapters.push((protocol, Box::new(adapter)));
        self
    }

    pub fn adapter_for(&self, protocol: ProviderProtocol) -> Option<&dyn ProviderAdapter> {
        self.adapters
            .iter()
            .find(|(registered, _)| *registered == protocol)
            .map(|(_, adapter)| adapter.as_ref())
    }
}
