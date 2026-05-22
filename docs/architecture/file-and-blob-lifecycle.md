# Plan: Unified lifecycle hooks (file + blob)

## Context

In order to help new features integration and a good hygien on objects (example: reduce risk of orphean entries)
Lifecycle traits are added

---

## Design decisions

TODO: draw a mermaid

### Synchronous trait methods

Hooks are fire-and-notify: every implementation either spawns a `tokio::spawn` (important to avoid blocking implementation)
internally or does nothing. Sync trait = no `Box::pin`, no `async_trait`, genuine one-liner noops.

```rust
// application/ports/file_lifecycle.rs
pub trait FileLifecycleHook: Send + Sync {

    // fired on file created (after blob is written), is_new_blob tells if this blob was already present
    fn on_file_created(&self, file_id: &str, blob_hash: &str, content_type: &str, is_new_blob: bool);

    // fired on file updated, means that blob changed
    fn on_file_updated(&self, file_id: &str, blob_hash: &str, content_type: &str);

    // fired on copy, the blob remains unchanged
    fn on_file_copied(&self, file_id: &str, blob_hash: &str, content_type: &str, source_id: &str)

    // fired on file deletion, not information if the blob still exists, up to the implementor to use BlobLifecycleHook if needed
    fn on_file_deleted(&self, file_id: &str);
}

// application/ports/blob_lifecycle.rs
pub trait BlobLifecycleHook: Send + Sync {

    // a blob as been created
    fn on_blob_created(&self, blob_hash: &str, content_type: Option<&str>);

    // a blob as been deleted (no more refernce on it)
    fn on_blob_deleted(&self, blob_hash: &str);
}
```

**No default methods** — explicit noops required (forces developer acknowledgement of all events, if a method is not necessary, just implement it with a noop method).

### `is_new_blob: bool` on `on_file_created`

Tells the implementor whether the underlying blob is genuinely new (fresh upload, no dedup hit) or already existed (copy, dedup hit on re-upload). This prevents implementors from re-scanning/re-generating work that can be shared or cloned from an existing record:

use cases:

- `ThumbnailRefreshHook`: if `!is_new_blob`, the `blob_hash` thumbnail already exists on disk — skip scheduling generation entirely as server side thumbnail are based on blob
- `AudioMetadataService`: if `!is_new_blob`, clone the existing metadata row for the `blob_hash` (fast DB copy) instead of re-parsing the blob.

**Where `is_new_blob` comes from**: `FileUploadService` gets the dedup result from `FileBlobWriteRepository.save_file_from_temp()` (already computed during upload). For `copy_file()`, always `false` — same blob by definition.

