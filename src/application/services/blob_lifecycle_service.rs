use std::sync::Arc;

use crate::application::ports::blob_lifecycle::BlobLifecycleHook;

/// Composite dispatcher for blob lifecycle events.
///
/// Aggregates all [`BlobLifecycleHook`] implementations and fans out each
/// event to every registered handler. Services hold a single
/// `Arc<BlobLifecycleService>` — new handlers are added once, in DI, without
/// touching the services themselves.
pub struct BlobLifecycleService {
    hooks: Vec<Arc<dyn BlobLifecycleHook>>,
}

impl Default for BlobLifecycleService {
    fn default() -> Self {
        Self::new()
    }
}

impl BlobLifecycleService {
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    pub fn with_hook(mut self, hook: Arc<dyn BlobLifecycleHook>) -> Self {
        self.hooks.push(hook);
        self
    }
}

impl BlobLifecycleHook for BlobLifecycleService {
    fn on_blob_created(&self, blob_hash: &str, content_type: Option<&str>) {
        for hook in &self.hooks {
            hook.on_blob_created(blob_hash, content_type);
        }
    }

    fn on_blob_deleted(&self, blob_hash: &str) {
        for hook in &self.hooks {
            hook.on_blob_deleted(blob_hash);
        }
    }
}
