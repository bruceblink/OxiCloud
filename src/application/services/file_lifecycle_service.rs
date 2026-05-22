use std::sync::Arc;

use crate::application::ports::file_lifecycle::FileLifecycleHook;

/// Composite dispatcher for file lifecycle events.
///
/// Aggregates all [`FileLifecycleHook`] implementations and fans out each
/// event to every registered handler. Services hold a single
/// `Arc<FileLifecycleService>` — new handlers are added once, in DI, without
/// touching the services themselves.
pub struct FileLifecycleService {
    hooks: Vec<Arc<dyn FileLifecycleHook>>,
}

impl Default for FileLifecycleService {
    fn default() -> Self {
        Self::new()
    }
}

impl FileLifecycleService {
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    pub fn with_hook(mut self, hook: Arc<dyn FileLifecycleHook>) -> Self {
        self.hooks.push(hook);
        self
    }
}

impl FileLifecycleHook for FileLifecycleService {
    fn on_file_created(
        &self,
        file_id: &str,
        blob_hash: &str,
        content_type: &str,
        is_new_blob: bool,
    ) {
        for hook in &self.hooks {
            hook.on_file_created(file_id, blob_hash, content_type, is_new_blob);
        }
    }

    fn on_file_copied(
        &self,
        file_id: &str,
        blob_hash: &str,
        content_type: &str,
        source_file_id: &str,
    ) {
        for hook in &self.hooks {
            hook.on_file_copied(file_id, blob_hash, content_type, source_file_id);
        }
    }

    fn on_file_updated(&self, file_id: &str, blob_hash: &str, content_type: &str) {
        for hook in &self.hooks {
            hook.on_file_updated(file_id, blob_hash, content_type);
        }
    }

    fn on_file_deleted(&self, file_id: &str) {
        for hook in &self.hooks {
            hook.on_file_deleted(file_id);
        }
    }
}
