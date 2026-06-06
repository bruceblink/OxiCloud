use uuid::Uuid;

use crate::domain::services::path_service::{StoragePath, validate_storage_name};

// Re-export entity errors from the centralized module
pub use super::entity_errors::{FolderError, FolderResult};

/// Represents a folder entity in the domain
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// Unique identifier for the folder
    id: String,

    /// Name of the folder
    name: String,

    /// Path to the folder in the domain model
    storage_path: StoragePath,

    /// String representation of the path (for API compatibility)
    path_string: String,

    /// Parent folder ID (None if it's a root folder)
    parent_id: Option<String>,

    /// Owner user ID — scopes folder visibility per user.
    /// `None` only for legacy/stub folders; real folders always have an owner.
    owner_id: Option<Uuid>,

    /// Creation timestamp
    created_at: u64,

    /// Last modification timestamp of THIS folder row (rename, move,
    /// metadata change). Does NOT bump when descendants change —
    /// that signal lives on `tree_modified_at`.
    modified_at: u64,

    /// Latest `modified_at`-equivalent across the entire descendant
    /// subtree. Bumped by a PostgreSQL trigger on any file or folder
    /// write under this folder's ltree subtree. Source of the
    /// HTTP ETag emitted in PROPFIND/GET/HEAD responses — see
    /// [`Folder::etag`] for the formula and rationale.
    tree_modified_at: u64,
}

// We no longer need this module, now we use a String directly

impl Default for Folder {
    fn default() -> Self {
        Self {
            id: "stub-id".to_string(),
            name: "stub-folder".to_string(),
            storage_path: StoragePath::from_string("/"),
            path_string: "/".to_string(),
            parent_id: None,
            owner_id: None,
            created_at: 0,
            modified_at: 0,
            tree_modified_at: 0,
        }
    }
}

impl Folder {
    /// Creates a new folder with validation
    pub fn new(
        id: String,
        name: String,
        storage_path: StoragePath,
        parent_id: Option<String>,
    ) -> FolderResult<Self> {
        Self::new_with_owner(id, name, storage_path, parent_id, None)
    }

    /// Creates a new folder with validation and an explicit owner.
    pub fn new_with_owner(
        id: String,
        name: String,
        storage_path: StoragePath,
        parent_id: Option<String>,
        owner_id: Option<Uuid>,
    ) -> FolderResult<Self> {
        // Validate folder name
        if let Err(reason) = validate_storage_name(&name) {
            return Err(FolderError::InvalidFolderName(format!("{name}: {reason}")));
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Store the path string for serialization compatibility
        let path_string = storage_path.to_string();

        Ok(Self {
            id,
            name,
            storage_path,
            path_string,
            parent_id,
            owner_id,
            created_at: now,
            modified_at: now,
            tree_modified_at: now,
        })
    }

    /// Creates a folder with specific timestamps (for reconstruction).
    /// `tree_modified_at` defaults to `modified_at` — appropriate for
    /// in-memory construction; database loads should always go via
    /// [`Folder::with_timestamps_and_tree`] so the rollup value
    /// reflects DB reality.
    pub fn with_timestamps(
        id: String,
        name: String,
        storage_path: StoragePath,
        parent_id: Option<String>,
        created_at: u64,
        modified_at: u64,
    ) -> FolderResult<Self> {
        Self::with_timestamps_and_tree(
            id,
            name,
            storage_path,
            parent_id,
            None,
            created_at,
            modified_at,
            modified_at,
        )
    }

    /// Creates a folder with specific timestamps and owner (legacy
    /// constructor — `tree_modified_at` defaults to `modified_at`).
    /// Prefer [`Folder::with_timestamps_and_tree`] for DB reconstruction
    /// so the rollup ETag reflects descendant activity, not just this
    /// row's own metadata.
    pub fn with_timestamps_and_owner(
        id: String,
        name: String,
        storage_path: StoragePath,
        parent_id: Option<String>,
        owner_id: Option<Uuid>,
        created_at: u64,
        modified_at: u64,
    ) -> FolderResult<Self> {
        Self::with_timestamps_and_tree(
            id,
            name,
            storage_path,
            parent_id,
            owner_id,
            created_at,
            modified_at,
            modified_at,
        )
    }

    /// Full constructor used by the PG repository when reading rows.
    /// `tree_modified_at` comes from the trigger-maintained column on
    /// `storage.folders` and feeds [`Folder::etag`].
    #[allow(clippy::too_many_arguments)]
    pub fn with_timestamps_and_tree(
        id: String,
        name: String,
        storage_path: StoragePath,
        parent_id: Option<String>,
        owner_id: Option<Uuid>,
        created_at: u64,
        modified_at: u64,
        tree_modified_at: u64,
    ) -> FolderResult<Self> {
        if let Err(reason) = validate_storage_name(&name) {
            return Err(FolderError::InvalidFolderName(format!("{name}: {reason}")));
        }

        let path_string = storage_path.to_string();

        Ok(Self {
            id,
            name,
            storage_path,
            path_string,
            parent_id,
            owner_id,
            created_at,
            modified_at,
            tree_modified_at,
        })
    }

    // Getters
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn storage_path(&self) -> &StoragePath {
        &self.storage_path
    }

    pub fn path_string(&self) -> &str {
        &self.path_string
    }

    pub fn parent_id(&self) -> Option<&str> {
        self.parent_id.as_deref()
    }

    pub fn created_at(&self) -> u64 {
        self.created_at
    }

    pub fn modified_at(&self) -> u64 {
        self.modified_at
    }

    pub fn owner_id(&self) -> Option<Uuid> {
        self.owner_id
    }

    /// Latest descendant-write timestamp, maintained by a Postgres
    /// trigger that walks the ltree ancestor chain on every file or
    /// folder write inside this folder's subtree. See migration
    /// `20260625000000_folder_tree_modified_at.sql` for the trigger
    /// definition.
    pub fn tree_modified_at(&self) -> u64 {
        self.tree_modified_at
    }

    /// Opaque HTTP ETag string (raw, NOT HTTP-quoted). Handlers wrap
    /// in `"…"` themselves at the HTTP boundary.
    ///
    /// **Formula**: `{id[..16]}-{tree_modified_at}`.
    ///
    /// - The 16-char UUID prefix gives the folder its identity
    ///   component — keeps two empty same-mtime folders distinct.
    /// - `tree_modified_at` (Unix seconds) is the actual signal:
    ///   bumped by trigger whenever ANY descendant (file or
    ///   sub-folder, at any depth) is created, modified, deleted,
    ///   or moved. This is the contract NextCloud's sync engine
    ///   relies on — "did anything change inside this collection
    ///   since I last looked?". Until this column existed, the
    ///   answer was always "no" because the folder UUID never
    ///   changed; clients had to do periodic deep PROPFIND walks
    ///   to discover web-uploaded files.
    /// - Renaming the folder itself does NOT change the etag's
    ///   identity portion (UUID is stable across renames). The
    ///   trigger does bump `tree_modified_at` on rename via the
    ///   folder-side trigger, so the etag still changes — which is
    ///   correct, the parent collection's listing changed.
    pub fn etag(&self) -> String {
        let prefix: String = self.id.chars().take(16).collect();
        format!("{}-{}", prefix, self.tree_modified_at)
    }

    /// Creates a new Folder instance from a DTO
    /// This function is primarily for conversions in batch handlers
    pub fn from_dto(
        id: String,
        name: String,
        path: String,
        parent_id: Option<String>,
        created_at: u64,
        modified_at: u64,
    ) -> Self {
        // Create storage_path from the string
        let storage_path = StoragePath::from_string(&path);

        // Create directly without validation to avoid errors in DTO
        // conversions. `tree_modified_at` defaults to `modified_at`:
        // DTO round-trips lose the real rollup signal, so callers
        // that need a freshly-rolled-up etag must reload from the
        // repository.
        Self {
            id,
            name,
            storage_path,
            path_string: path,
            parent_id,
            owner_id: None,
            created_at,
            modified_at,
            tree_modified_at: modified_at,
        }
    }

    // Methods to create new versions of the folder (immutable)

    /// Creates a new version of the folder with updated name
    pub fn with_name(&self, new_name: String) -> FolderResult<Self> {
        if let Err(reason) = validate_storage_name(&new_name) {
            return Err(FolderError::InvalidFolderName(format!(
                "{new_name}: {reason}"
            )));
        }

        // Update path based on the name
        let parent_path = self.storage_path.parent();
        let new_storage_path = match parent_path {
            Some(parent) => parent.join(&new_name),
            None => StoragePath::from_string(&new_name),
        };

        // Update string representation
        let new_path_string = new_storage_path.to_string();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Ok(Self {
            id: self.id.clone(),
            name: new_name,
            storage_path: new_storage_path,
            path_string: new_path_string,
            parent_id: self.parent_id.clone(),
            owner_id: self.owner_id,
            created_at: self.created_at,
            modified_at: now,
            // Renaming bumps both self and descendant rollup —
            // ancestors' listings now show a new name, so the
            // collection has materially changed.
            tree_modified_at: now,
        })
    }

    /// Creates a new version of the folder with updated parent
    pub fn with_parent(
        &self,
        parent_id: Option<String>,
        parent_path: Option<StoragePath>,
    ) -> FolderResult<Self> {
        // We need a folder path to update the path
        let new_storage_path = match parent_path {
            Some(path) => path.join(&self.name),
            None => StoragePath::from_string(&self.name), // Root
        };

        // Update string representation
        let new_path_string = new_storage_path.to_string();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Ok(Self {
            id: self.id.clone(),
            name: self.name.clone(),
            storage_path: new_storage_path,
            path_string: new_path_string,
            parent_id,
            owner_id: self.owner_id,
            created_at: self.created_at,
            modified_at: now,
            tree_modified_at: now,
        })
    }

    /// Returns an absolute path for this folder
    pub fn get_absolute_path<P: AsRef<std::path::Path>>(&self, root_path: P) -> std::path::PathBuf {
        let mut result = std::path::PathBuf::from(root_path.as_ref());

        // Skip leading '/' from path_string to avoid creating absolute path incorrectly
        let relative_path = if self.path_string.starts_with('/') {
            &self.path_string[1..]
        } else {
            &self.path_string
        };

        if !relative_path.is_empty() {
            result.push(relative_path);
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_folder_creation_with_valid_name() {
        let storage_path = StoragePath::from_string("/test/folder");
        let folder = Folder::new(
            "123".to_string(),
            "my_folder".to_string(),
            storage_path,
            None,
        );

        assert!(folder.is_ok());
    }

    #[test]
    fn test_folder_creation_with_invalid_name() {
        let storage_path = StoragePath::from_string("/test/invalid/folder");
        let folder = Folder::new(
            "123".to_string(),
            "folder/with/slash".to_string(), // Invalid name
            storage_path,
            None,
        );

        assert!(folder.is_err());
        match folder {
            Err(FolderError::InvalidFolderName(_)) => (),
            _ => panic!("Expected InvalidFolderName error"),
        }
    }

    #[test]
    fn test_folder_with_name() {
        let storage_path = StoragePath::from_string("/test/folder");
        let folder = Folder::new(
            "123".to_string(),
            "old_name".to_string(),
            storage_path,
            None,
        )
        .unwrap();

        let renamed = folder.with_name("new_name".to_string());
        assert!(renamed.is_ok());
        let renamed = renamed.unwrap();
        assert_eq!(renamed.name(), "new_name");
        assert_eq!(renamed.id(), "123"); // The ID doesn't change
    }

    /// The folder ETag is `{id[..16]}-{tree_modified_at}`. Two
    /// fixtures with identical id-prefix + tree_modified_at must
    /// produce byte-identical ETags — that's what NC's incremental
    /// sync relies on across PROPFIND cycles.
    #[test]
    fn test_etag_combines_id_prefix_and_tree_modified_at() {
        let folder = Folder::with_timestamps_and_tree(
            "0123456789abcdefZZZZZZZZ".to_string(),
            "folder".to_string(),
            StoragePath::from_string("/folder"),
            None,
            None,
            1_000,
            2_000,
            5_000,
        )
        .unwrap();

        assert_eq!(folder.tree_modified_at(), 5_000);
        assert_eq!(folder.etag(), "0123456789abcdef-5000");
    }

    /// Two folders with the same `tree_modified_at` but different
    /// IDs must NOT collide on ETag — the id prefix is the identity
    /// portion that keeps them distinct.
    #[test]
    fn test_etag_distinct_folders_same_tree_mtime() {
        let a = Folder::with_timestamps_and_tree(
            "aaaaaaaaaaaaaaaaZZZZZZZZ".to_string(),
            "a".to_string(),
            StoragePath::from_string("/a"),
            None,
            None,
            0,
            0,
            42,
        )
        .unwrap();
        let b = Folder::with_timestamps_and_tree(
            "bbbbbbbbbbbbbbbbZZZZZZZZ".to_string(),
            "b".to_string(),
            StoragePath::from_string("/b"),
            None,
            None,
            0,
            0,
            42,
        )
        .unwrap();

        assert_ne!(a.etag(), b.etag());
    }

    /// `tree_modified_at` is the actual change-detection signal —
    /// the trigger bumps it for descendant writes. Renaming the
    /// folder bumps both `modified_at` and `tree_modified_at`
    /// (the parent collection's listing changed), and the etag
    /// must reflect that — otherwise NC won't notice the rename.
    #[test]
    fn test_etag_changes_when_tree_modified_at_changes() {
        let folder_a = Folder::with_timestamps_and_tree(
            "abcd1234efgh5678ZZZZZZZZ".to_string(),
            "folder".to_string(),
            StoragePath::from_string("/folder"),
            None,
            None,
            1_000,
            2_000,
            3_000,
        )
        .unwrap();
        let folder_b = Folder::with_timestamps_and_tree(
            "abcd1234efgh5678ZZZZZZZZ".to_string(),
            "folder".to_string(),
            StoragePath::from_string("/folder"),
            None,
            None,
            1_000,
            2_000,
            4_000,
        )
        .unwrap();

        assert_ne!(folder_a.etag(), folder_b.etag());
    }
}
