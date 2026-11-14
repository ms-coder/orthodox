//! Maps a [`Location`] to its provider. Remote providers (v1.1+) register here; nothing else in
//! the code base may assume "everything is local" outside `Location::Local` handling.

use std::collections::HashMap;
use std::sync::Arc;

use ofm_core::{Location, LocationKind};

use crate::error::VfsError;
use crate::local::LocalVfs;
use crate::vfs::Vfs;

/// Provider lookup table; cheap to share behind an `Arc`.
#[derive(Default, Clone)]
pub struct VfsRegistry {
    providers: HashMap<LocationKind, Arc<dyn Vfs>>,
}

impl VfsRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry with the local provider installed (the app adds archive/list providers via
    /// [`register`](Self::register) when those milestones land).
    pub fn with_local() -> Self {
        let mut r = Self::new();
        r.register(LocationKind::Local, Arc::new(LocalVfs::new()));
        r
    }

    /// Install (or replace) the provider for a kind of location.
    pub fn register(&mut self, kind: LocationKind, vfs: Arc<dyn Vfs>) {
        self.providers.insert(kind, vfs);
    }

    /// Provider responsible for `loc`.
    pub fn get(&self, loc: &Location) -> Result<Arc<dyn Vfs>, VfsError> {
        self.providers
            .get(&loc.kind())
            .cloned()
            .ok_or_else(|| VfsError::NoProvider(loc.display_lossy()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_provider_is_an_error() {
        let r = VfsRegistry::new();
        assert!(matches!(
            r.get(&Location::Local("/".into())),
            Err(VfsError::NoProvider(_))
        ));
        assert!(
            VfsRegistry::with_local()
                .get(&Location::Local("/".into()))
                .is_ok()
        );
    }
}
