//! Repository for ReBAC subject groups.
//!
//! See `src/domain/entities/subject_group.rs` for the entity and
//! `migrations/20260612000000_subject_groups.sql` for the schema.

use std::collections::HashSet;

use thiserror::Error;
use uuid::Uuid;

use crate::domain::entities::subject_group::{GroupMember, SubjectGroup};

#[derive(Debug, Error)]
pub enum SubjectGroupRepositoryError {
    #[error("Group not found: {0}")]
    NotFound(String),
    #[error("Group with name already exists: {0}")]
    NameAlreadyExists(String),
    #[error("Member already in group")]
    MemberAlreadyPresent,
    #[error("Member not in group")]
    MemberNotPresent,
    /// Attempting to add a group-member that would create a cycle.
    #[error("Adding this member would create a cycle: {0}")]
    Cycle(String),
    /// Attempting to add a group-member that would exceed `MAX_GROUP_DEPTH`.
    #[error("Adding this member would exceed the maximum nesting depth: {0}")]
    DepthExceeded(String),
    /// Attempt to mutate the immutable `Internal` virtual group.
    #[error("Virtual groups cannot be modified: {0}")]
    VirtualImmutable(String),
    /// Group name fails RFC 5321 local-part validation (mirrored at the DB
    /// via a CHECK constraint).
    #[error("Invalid group name: {0}")]
    InvalidName(String),
    #[error("Storage error: {0}")]
    StorageError(String),
}

pub trait SubjectGroupRepository: Send + Sync + 'static {
    /// Create a new (non-virtual) group. Fails with `NameAlreadyExists` if
    /// the name (case-insensitive) is taken, or `InvalidName` if the DB
    /// CHECK rejects the shape.
    async fn create(
        &self,
        group: &SubjectGroup,
    ) -> Result<SubjectGroup, SubjectGroupRepositoryError>;

    /// Fetch a group by primary key. Returns `None` if missing — callers
    /// decide whether absence is an error.
    async fn get_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<SubjectGroup>, SubjectGroupRepositoryError>;

    /// Fetch a group by name. `CITEXT` makes this case-insensitive.
    async fn get_by_name(
        &self,
        name: &str,
    ) -> Result<Option<SubjectGroup>, SubjectGroupRepositoryError>;

    /// List groups; `name_query` is a substring match (ILIKE) when provided.
    /// Returns `(rows, total)` for pagination UIs.
    async fn list(
        &self,
        limit: u32,
        offset: u32,
        name_query: Option<&str>,
    ) -> Result<(Vec<SubjectGroup>, u64), SubjectGroupRepositoryError>;

    /// Same as `list`, but each row is paired with its direct-member count.
    /// Used by the management UI to show "(N members)" on each row without
    /// the N+1 query of calling `count_members` per row. The count is the
    /// LEFT-JOIN aggregate from `auth.subject_group_members` so the
    /// implementation pulls everything in a single SQL round-trip.
    async fn list_with_counts(
        &self,
        limit: u32,
        offset: u32,
        name_query: Option<&str>,
    ) -> Result<(Vec<(SubjectGroup, i64)>, u64), SubjectGroupRepositoryError>;

    /// Count direct members (users + nested groups) of a single group.
    /// Used by single-item endpoints (create / get / update) so the response
    /// DTO can include `member_count` without a separate frontend round-trip.
    async fn count_members(&self, id: Uuid) -> Result<i64, SubjectGroupRepositoryError>;

    /// Rename the group. Fails on collision or invalid shape.
    async fn rename(
        &self,
        id: Uuid,
        new_name: &str,
    ) -> Result<SubjectGroup, SubjectGroupRepositoryError>;

    /// Delete the group. Cascades to `subject_group_members` and to
    /// `storage.access_grants` rows referencing this group as subject (via
    /// the application service — there is no FK between `access_grants` and
    /// `subject_groups`, so the service performs the cascade explicitly in
    /// the same transaction).
    async fn delete(&self, id: Uuid) -> Result<(), SubjectGroupRepositoryError>;

    /// Add a member (user or another group). Performs cycle + depth checks
    /// inside the same transaction (`SELECT ... FOR UPDATE` on the parent
    /// row to prevent racing concurrent adds from squeezing under the limit
    /// individually).
    async fn add_member(
        &self,
        group_id: Uuid,
        member: GroupMember,
        added_by: Uuid,
    ) -> Result<(), SubjectGroupRepositoryError>;

    /// Remove a member. No-op-safe: returns `MemberNotPresent` if the row
    /// doesn't exist.
    async fn remove_member(
        &self,
        group_id: Uuid,
        member: GroupMember,
    ) -> Result<(), SubjectGroupRepositoryError>;

    /// Direct members of `group_id` (one level only, not transitive).
    async fn list_direct_members(
        &self,
        group_id: Uuid,
    ) -> Result<Vec<GroupMember>, SubjectGroupRepositoryError>;

    /// All users transitively in `group_id` (debug / audit / admin views).
    async fn list_transitive_users(
        &self,
        group_id: Uuid,
    ) -> Result<Vec<Uuid>, SubjectGroupRepositoryError>;

    /// All groups `user_id` belongs to transitively. This is the hot path
    /// driven by `PgAclEngine::expand_subject` on every cache miss — the
    /// `Internal` virtual group is NOT included here (the engine adds it).
    async fn groups_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<HashSet<Uuid>, SubjectGroupRepositoryError>;
}
