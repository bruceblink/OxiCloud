# Plan: Unified lifecycle hooks (file + blob)

## Context

The file and blob lifecycle hook systems are partially built but inconsistently wired:
- `FileLifecycleService` only fans out `on_file_deleted`; created/updated hooks are wired directly on `FileUploadService`.
- `AudioMetadataService` implements no hook traits — called raw from 4 handler files.
- `ThumbnailRefreshHook` (file created/updated) and `ThumbnailService` (file deleted, blob deleted) are separate registrations for the same concern.
- `copy_file()` fires no hooks — copied files never get audio metadata (confirmed gap: `audio.file_metadata` is keyed by `file_id`, not `blob_hash`).
- Blob lifecycle has the same structural problem: two separate traits and two separate vecs in `DedupService`.

Goal: one `FileLifecycleHook` + one `BlobLifecycleHook` trait, each with a composite dispatcher, all side-effects wired through them, handlers reduced to protocol translators.

---

## Design decisions

### Synchronous trait methods

Hooks are fire-and-notify: every implementation either spawns a `tokio::spawn` internally or does nothing. Sync trait = no `Box::pin`, no `async_trait`, genuine one-liner noops.

```rust
// application/ports/file_lifecycle.rs
pub trait FileLifecycleHook: Send + Sync {
    fn on_file_created(&self, file_id: &str, blob_hash: &str, content_type: &str, is_new_blob: bool);
    fn on_file_updated(&self, file_id: &str, blob_hash: &str, content_type: &str);
    fn on_file_copied(&self, file_id: &str, blob_hash: &str, content_type: &str, source_id: &str)
    // not information if the blob still exists, up to implementor to use BlobLifecycleHook if needed
    fn on_file_deleted(&self, file_id: &str);
}

// application/ports/blob_lifecycle.rs
pub trait BlobLifecycleHook: Send + Sync {
    fn on_blob_created(&self, blob_hash: &str, content_type: Option<&str>);
    fn on_blob_deleted(&self, blob_hash: &str);
}
```

**No default methods** — explicit noops required (forces developer acknowledgement of all events).

### `is_new_blob: bool` on `on_file_created`

Tells the implementor whether the underlying blob is genuinely new (fresh upload, no dedup hit) or already existed (copy, dedup hit on re-upload). This prevents implementors from re-scanning/re-generating work that can be shared or cloned from an existing record:

- `ThumbnailRefreshHook`: if `!is_new_blob`, the `blob_hash` thumbnail already exists on disk — skip scheduling generation entirely.
- `AudioMetadataService`: if `!is_new_blob`, clone the existing metadata row for the `blob_hash` (fast DB copy) instead of re-parsing the blob.

**Where `is_new_blob` comes from**: `FileUploadService` gets the dedup result from `FileBlobWriteRepository.save_file_from_temp()` (already computed during upload). For `copy_file()`, always `false` — same blob by definition.

### Old traits removed entirely

Six old traits (`FileCreatedHook`, `FileUpdatedHook`, `FileDeletedHook`, `BlobCreationHook`, `BlobDeletionHook`) removed. All implementors migrate to the two new traits.

### Why `on_file_deleted` can be sync

`ThumbnailService.delete_thumbnails` is currently awaited by the caller. It moves to `tokio::spawn` internally — thumbnail cleanup is best-effort, callers don't depend on it completing.

---

## Thumbnail storage model (context)

- **Disk**: keyed by `blob_hash` → `thumbnails_root/{size}/{blob_hash}.jpg` — shared between all files with the same content.
- **Moka cache**: keyed by `(file_id, size)` — cold-misses on first request for a new `file_id`, then reads from disk.
- **External thumbnails** (video frames): keyed by `file_id` → `ext-{file_id}.jpg`.

Image copy is safe: disk thumbnail exists for the `blob_hash`, no regeneration needed (`is_new_blob = false` will skip it).

---

## Files to change

### 1. `src/application/ports/file_lifecycle.rs`
Replace three separate async traits with one sync `FileLifecycleHook` trait (3 methods + `is_new_blob` on created, no defaults).

### 2. `src/application/ports/blob_lifecycle.rs`
Replace two separate async traits with one sync `BlobLifecycleHook` trait (2 methods, no defaults).

### 3. `src/application/services/file_lifecycle_service.rs`
- One `Vec<Arc<dyn FileLifecycleHook>>`.
- One builder: `.with_hook(hook)`.
- `impl FileLifecycleHook`: plain `for` loops, no async, forwards `is_new_blob`.

### 4. New: `src/application/services/blob_lifecycle_service.rs`
Mirror of `FileLifecycleService` for blob events:
- `Vec<Arc<dyn BlobLifecycleHook>>`, `.with_hook()` builder, `impl BlobLifecycleHook` fan-out.

### 5. `src/infrastructure/services/thumbnail_service.rs`
Consolidate all thumbnail hook logic into `ThumbnailRefreshHook`, implementing **both** new traits:

**`impl FileLifecycleHook for ThumbnailRefreshHook`**:
- `on_file_created`: if `!is_new_blob` or unsupported content type → return early (blob thumbnail already on disk). Otherwise spawn generation.
- `on_file_updated`: spawn thumbnail invalidation + regeneration (existing logic).
- `on_file_deleted`: `tokio::spawn({ thumbnail.delete_thumbnails(file_id).await })`.

**`impl BlobLifecycleHook for ThumbnailRefreshHook`**:
- `on_blob_created`: explicit noop — thumbnail gen is handled at file level via `on_file_created`.
- `on_blob_deleted`: `tokio::spawn({ thumbnail.delete_blob_thumbnails(blob_hash).await })`.

Remove: `impl FileDeletedHook for ThumbnailService`, `impl BlobDeletionHook for ThumbnailService`.

### 6. `src/infrastructure/services/audio_metadata_service.rs`

**New method**: `clone_or_extract_background(service: Arc<Self>, new_file_id: Uuid, blob_hash: String)`
- Spawns a task that runs:
  ```sql
  INSERT INTO audio.file_metadata (file_id, title, artist, album, album_artist,
      genre, track_number, disc_number, year, duration_secs, format)
  SELECT $new_file_id, title, artist, album, album_artist, genre,
      track_number, disc_number, year, duration_secs, format
  FROM audio.file_metadata am
  JOIN storage.files sf ON sf.id = am.file_id
  WHERE sf.blob_hash = $blob_hash
  LIMIT 1
  ON CONFLICT (file_id) DO NOTHING
  ```
- If 0 rows inserted (original not yet processed), falls back to `extract_and_save`.

**`impl FileLifecycleHook for AudioMetadataService`**:
- `on_file_created`: if `is_audio_file(content_type)` → parse UUID, then:
  - `is_new_blob = true` → `spawn_extraction_background(file_id, blob_path(blob_hash))`
  - `is_new_blob = false` → `clone_or_extract_background(file_id, blob_hash)`
- `on_file_updated`: if audio → `spawn_extraction_with_delete_background`.
- `on_file_deleted`: explicit one-liner noop + comment: `audio.file_metadata` has `ON DELETE CASCADE`, DB handles cleanup.

### 7. `src/application/services/file_upload_service.rs`
- Replace `file_created_hooks: Vec<Arc<dyn FileCreatedHook>>` + `file_updated_hooks` with `file_lifecycle_hook: Option<Arc<dyn FileLifecycleHook>>`.
- Builder: `.with_file_lifecycle_hook(hook)`.
- Sync calls replacing async fan-out loops. Pass `is_new_blob` from the dedup result already available at this layer.

### 8. `src/application/services/file_management_service.rs`
- Replace `Arc<dyn FileDeletedHook>` with `Arc<dyn FileLifecycleHook>`.
- `on_file_deleted` call becomes sync.
- **Fix copy gap**: after `file_repository.copy_file()` returns the new file DTO, call `self.file_lifecycle.on_file_created(new_id, blob_hash, mime_type, false)`.

### 9. `src/application/services/trash_service.rs`
- Replace `Arc<dyn FileDeletedHook>` with `Arc<dyn FileLifecycleHook>`.
- `on_file_deleted` calls become sync.

### 10. `src/infrastructure/services/dedup_service.rs`
- Replace `blob_creation_hooks: Vec<Arc<dyn BlobCreationHook>>` + `blob_hooks: Vec<Arc<dyn BlobDeletionHook>>` with `blob_lifecycle: Option<Arc<BlobLifecycleService>>`.
- Builder: `.with_blob_lifecycle(hook)`.
- Sync calls replacing async fan-outs.

### 11. `src/common/di.rs`

```rust
let thumbnail_hook = Arc::new(ThumbnailRefreshHook::new(
    core.thumbnail_service.clone(),
    dedup.clone(),
));

let file_lifecycle = Arc::new(
    FileLifecycleService::new()
        .with_hook(thumbnail_hook.clone())
        .with_hook(audio_metadata_service.clone())  // if Some
);

let blob_lifecycle = Arc::new(
    BlobLifecycleService::new()
        .with_hook(thumbnail_hook.clone())
);

dedup_service.with_blob_lifecycle(blob_lifecycle)
file_upload_service.with_file_lifecycle_hook(file_lifecycle.clone())
file_management_service.with_file_lifecycle_hook(file_lifecycle.clone())
trash_service.with_file_lifecycle_hook(file_lifecycle.clone())
```

### 12. Handler cleanup — 4 files (deletes only)

| File | Remove |
|---|---|
| `src/interfaces/api/handlers/file_handler.rs` | direct `thumbnail_service.generate_all_sizes_background_from_bytes(...)` + `AudioMetadataService::spawn_extraction_background(...)` |
| `src/interfaces/nextcloud/webdav_handler.rs` | `AudioMetadataService::spawn_extraction_background(...)` (create) + `AudioMetadataService::spawn_extraction_with_delete_background(...)` (update) |
| `src/interfaces/nextcloud/uploads_handler.rs` | `AudioMetadataService::spawn_extraction_background(...)` |
| `src/interfaces/api/handlers/webdav_handler.rs` | `AudioMetadataService::spawn_extraction_background(...)` |

---

## Execution order

1. `file_lifecycle.rs` — new trait
2. `blob_lifecycle.rs` — new trait
3. `file_lifecycle_service.rs` — updated composite
4. New `blob_lifecycle_service.rs`
5. `thumbnail_service.rs` — merged impl of both traits
6. `audio_metadata_service.rs` — new method + `FileLifecycleHook` impl
7. `file_upload_service.rs` — unified hook field + `is_new_blob` plumbing
8. `file_management_service.rs` — type update + copy hook
9. `trash_service.rs` — type update
10. `dedup_service.rs` — unified blob hook field
11. `di.rs` — rewire
12. Handler cleanups (4 files, independent)

---

## Verification

```bash
cargo fmt --all
cargo clippy --all-features --all-targets -- -D warnings   # zero warnings
cargo test --workspace                                      # all ~208 tests green
```

Smoke-test manually:
- Upload an image → thumbnail appears.
- Upload same image again (dedup hit) → no thumbnail re-generation.
- Upload an audio file via Nextcloud WebDAV → audio metadata present.
- Copy an image file → copy has thumbnail served instantly from blob_hash path.
- Copy a music file → copy has audio metadata (cloned row, no blob re-parse).
- Delete a file → thumbnails cleared; audio metadata gone (DB cascade).
- Overwrite file via WebDAV PUT → thumbnail refreshes.
- Delete last copy of a blob → blob-hash thumbnail file removed from disk.
