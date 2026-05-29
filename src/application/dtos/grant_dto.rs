//! DTOs for the ReBAC `/api/grants` REST endpoints.
//!
//! The wire shapes are intentionally separate from the domain types
//! (`Subject`, `Resource`, `Permission`, `Grant`) so that domain stays
//! storage-agnostic and DTOs can evolve with the HTTP contract.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::application::dtos::cursor::{CursorListResponse, CursorQuery, PageCursor};
use crate::application::dtos::file_dto::FileDto;
use crate::application::dtos::folder_dto::FolderDto;
use crate::domain::services::authorization::{Grant, Permission, Resource, Subject};

// ════════════════════════════════════════════════════════════════════════════
// Subject / Resource / Permission DTOs
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SubjectTypeDto {
    User,
    Group,
    Token,
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SubjectDto {
    #[serde(rename = "type")]
    pub kind: SubjectTypeDto,
    pub id: Uuid,
}

impl From<SubjectDto> for Subject {
    fn from(dto: SubjectDto) -> Self {
        match dto.kind {
            SubjectTypeDto::User => Subject::User(dto.id),
            SubjectTypeDto::Group => Subject::Group(dto.id),
            SubjectTypeDto::Token => Subject::Token(dto.id),
            SubjectTypeDto::External => Subject::External(dto.id),
        }
    }
}

impl From<Subject> for SubjectDto {
    fn from(s: Subject) -> Self {
        let (kind, id) = match s {
            Subject::User(id) => (SubjectTypeDto::User, id),
            Subject::Group(id) => (SubjectTypeDto::Group, id),
            Subject::Token(id) => (SubjectTypeDto::Token, id),
            Subject::External(id) => (SubjectTypeDto::External, id),
        };
        SubjectDto { kind, id }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ResourceTypeDto {
    Folder,
    File,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResourceDto {
    #[serde(rename = "type")]
    pub kind: ResourceTypeDto,
    pub id: Uuid,
}

impl From<ResourceDto> for Resource {
    fn from(dto: ResourceDto) -> Self {
        match dto.kind {
            ResourceTypeDto::Folder => Resource::Folder(dto.id),
            ResourceTypeDto::File => Resource::File(dto.id),
        }
    }
}

impl From<Resource> for ResourceDto {
    fn from(r: Resource) -> Self {
        let (kind, id) = match r {
            Resource::Folder(id) => (ResourceTypeDto::Folder, id),
            Resource::File(id) => (ResourceTypeDto::File, id),
        };
        ResourceDto { kind, id }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PermissionDto {
    Read,
    Create,
    Share,
    Comment,
    Delete,
    Update,
}

impl From<PermissionDto> for Permission {
    fn from(p: PermissionDto) -> Self {
        match p {
            PermissionDto::Read => Permission::Read,
            PermissionDto::Create => Permission::Create,
            PermissionDto::Share => Permission::Share,
            PermissionDto::Comment => Permission::Comment,
            PermissionDto::Delete => Permission::Delete,
            PermissionDto::Update => Permission::Update,
        }
    }
}

impl From<Permission> for PermissionDto {
    fn from(p: Permission) -> Self {
        match p {
            Permission::Read => PermissionDto::Read,
            Permission::Create => PermissionDto::Create,
            Permission::Share => PermissionDto::Share,
            Permission::Comment => PermissionDto::Comment,
            Permission::Delete => PermissionDto::Delete,
            Permission::Update => PermissionDto::Update,
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Roles (DTO-layer sugar)
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Viewer,
    //Commenter,
    Editor,
    //Manager,
    Admin,
}

impl Role {
    /// Expands a role into its constituent raw permissions. Storage and
    /// engine know nothing about roles — the server normalizes here before
    /// writing rows.
    pub fn expand(self) -> &'static [Permission] {
        match self {
            Role::Viewer => &[Permission::Read],
            /* reserved for future
            Role::Commenter => &[Permission::Read, Permission::Comment],
            */
            Role::Editor => &[
                Permission::Read,
                Permission::Comment,
                Permission::Create,
                Permission::Update,
            ],
            /* reserved for future
            Role::Manager => &[
                Permission::Read,
                Permission::Comment,
                Permission::Create,
                Permission::Update,
                Permission::Share,
            ],
            */
            Role::Admin => &[
                Permission::Read,
                Permission::Comment,
                Permission::Create,
                Permission::Update,
                Permission::Share,
                Permission::Delete,
            ],
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Request DTOs
// ════════════════════════════════════════════════════════════════════════════

/// `POST /api/grants` — accepts either `permissions` (explicit) or `role`.
/// Server-side validation requires exactly one of the two to be present.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateGrantDto {
    pub subject: SubjectDto,
    pub resource: ResourceDto,
    #[serde(default)]
    pub permissions: Option<Vec<PermissionDto>>,
    #[serde(default)]
    pub role: Option<Role>,
    /// Optional expiry for every grant in this request. RFC 3339 / ISO 8601.
    #[serde(default)]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// `PUT /api/grants/role` — reconcile a subject's role on a resource.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateRoleDto {
    pub subject: SubjectDto,
    pub resource: ResourceDto,
    pub role: Role,
    /// Optional expiry applied to every grant written or updated by this call.
    #[serde(default)]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

// ════════════════════════════════════════════════════════════════════════════
// Response DTOs
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GrantDto {
    pub id: Uuid,
    pub subject: SubjectDto,
    pub resource: ResourceDto,
    pub permission: PermissionDto,
    pub granted_by: Uuid,
    pub granted_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<Grant> for GrantDto {
    fn from(g: Grant) -> Self {
        Self {
            id: g.id,
            subject: g.subject.into(),
            resource: g.resource.into(),
            permission: g.permission.into(),
            granted_by: g.granted_by,
            granted_at: g.granted_at,
            expires_at: g.expires_at,
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Shared-with-me DTOs  (GET /api/grants/incoming/resources)
// ════════════════════════════════════════════════════════════════════════════

/// Query parameters for `GET /api/grants/incoming/resources`.
///
/// `limit`, `cursor`, and `sort_by` follow the standard [`CursorQuery`]
/// contract.  They are declared directly here rather than via
/// `#[serde(flatten)]` because `serde_urlencoded` (Axum's query extractor)
/// does not support flattening.
#[derive(Debug, Deserialize, IntoParams)]
pub struct SharedWithMeQuery {
    /// Maximum number of items to return (1–200, default 50).
    #[serde(default = "CursorQuery::default_limit")]
    pub limit: u32,
    /// Opaque cursor from a previous response. Omit to start from the
    /// most-recently-granted item.
    pub cursor: Option<String>,
    /// Sort dimension. Supported values: `"granted_at"` (default),
    /// `"granted_by"` (for swimlane grouping).
    pub sort_by: Option<String>,
    /// Comma-separated resource types to include, e.g. `file,folder`.
    /// Omit to return all known types.
    pub resource_types: Option<String>,
    /// Reverse the sort order. Default `false` (normal order).
    /// Must be the same on all pages of the same result set — the cursor
    /// carries this flag so the server can validate consistency.
    #[serde(default)]
    pub reverse: bool,
}

impl SharedWithMeQuery {
    /// Returns `limit` clamped to `[1, 200]`.
    pub fn limit_clamped(&self) -> usize {
        self.limit.clamp(1, 200) as usize
    }

    /// Decode the optional cursor string.  Invalid cursor → start from top.
    pub fn decode_cursor<C: PageCursor>(&self) -> Option<C> {
        self.cursor.as_deref().and_then(C::decode)
    }
}

/// The resource payload for one item in the shared-with-me list.
///
/// The variant is discriminated by `resource_type` on the parent
/// [`SharedWithMeItemDto`].  Serialised as the inner object (no wrapper key)
/// via `#[serde(untagged)]`, so consumers see the file/folder fields directly
/// under the `resource` key.
#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]
pub enum ResourceContentDto {
    File(FileDto),
    Folder(FolderDto),
}

/// One item in the shared-with-me list.
///
/// `resource_type` indicates whether `resource` contains a file or a folder.
/// Using a single `resource` field (instead of nullable `file`/`folder` pairs)
/// makes adding new resource types backward-compatible — only `resource_type`
/// gains a new variant; the wrapper shape stays the same.
#[derive(Debug, Serialize, ToSchema)]
pub struct SharedWithMeItemDto {
    pub resource_type: ResourceTypeDto,
    /// All permissions the caller holds on this resource (aggregated).
    pub permissions: Vec<PermissionDto>,
    /// Earliest grant date for this resource.
    pub granted_at: chrono::DateTime<chrono::Utc>,
    /// UUID of the user who created the (earliest) grant.
    pub granted_by: Uuid,
    /// Full resource details. Shape is determined by `resource_type`.
    pub resource: ResourceContentDto,
}

/// Derive the closest-matching role label from a set of permissions.
/// Maps the permission set to `"admin"`, `"editor"`, or `"viewer"`.
pub fn role_from_permissions(perms: &[Permission]) -> &'static str {
    if perms.contains(&Permission::Delete) && perms.contains(&Permission::Share) {
        "admin"
    } else if perms.contains(&Permission::Create) || perms.contains(&Permission::Update) {
        "editor"
    } else {
        "viewer"
    }
}

/// Response for `GET /api/grants/incoming/resources`.
pub type SharedWithMeDto = CursorListResponse<SharedWithMeItemDto>;

// ════════════════════════════════════════════════════════════════════════════
// My-Shares DTOs  (GET /api/grants/outgoing/resources)
// ════════════════════════════════════════════════════════════════════════════

/// One (subject, permissions) entry within an outgoing resource item.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OutgoingResourceGrantDto {
    pub grant_id: Uuid,
    /// `"user"` | `"token"`
    pub subject_type: String,
    pub subject_id: Uuid,
    /// Human-readable label (username for users, share name for tokens).
    pub subject_display: String,
    /// Derived role label: `"viewer"` | `"editor"` | `"admin"`.
    pub role: String,
    pub granted_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Whether the token has a password set. Always `false` for user subjects.
    pub has_password: bool,
}

/// One item in the my-shares list.
#[derive(Debug, Serialize, ToSchema)]
pub struct OutgoingResourceItemDto {
    pub resource_type: ResourceTypeDto,
    /// Earliest grant date across all subjects on this resource.
    pub first_shared_at: chrono::DateTime<chrono::Utc>,
    /// Full resource details. Shape is determined by `resource_type`.
    pub resource: ResourceContentDto,
    /// One entry per (subject, permissions) pair.
    pub grants: Vec<OutgoingResourceGrantDto>,
}

/// Response for `GET /api/grants/outgoing/resources`.
pub type MySharesDto = CursorListResponse<OutgoingResourceItemDto>;
