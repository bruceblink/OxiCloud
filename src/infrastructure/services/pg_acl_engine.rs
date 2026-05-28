//! PostgreSQL-backed implementation of `AuthorizationEngine`.
//!
//! Stores grants in `storage.access_grants` (see migration
//! `20260520000000_rebac_access_grants.sql`). Cascading is resolved at check
//! time via PostgreSQL `ltree` `@>` (ancestor-of) on `storage.folders.lpath`,
//! using the existing GiST index for O(log N) traversal.
//!
//! Owner is implicit — `storage.folders.user_id` / `storage.files.user_id`
//! are checked first via dedicated helpers; if the caller is the owner, no
//! SQL against `access_grants` happens.
//!
//! ## Lifecycle cleanup
//!
//! In v1, cleanup of grant rows when a resource or subject is permanently
//! deleted is enforced by **DB triggers** (`trg_cleanup_grants_*` in the
//! migration). The application layer does not call `revoke_all_for_*`
//! explicitly today — the triggers are the canonical path because they
//! also catch bulk SQL maintenance, admin scripts, and any code path that
//! bypasses the service layer.
//!
//! The `revoke_all_for_resource` / `revoke_all_for_subject` methods exist
//! on the trait for future use cases:
//! - **Caching** (planned) — a `CachedAuthorizationEngine` decorator needs
//!   to see the invalidation event at the engine boundary, not just at the
//!   SQL level. When caching lands, services will start calling these
//!   methods explicitly before/around delete operations.
//! - **Alternate engines** (OpenFGA, future) — engines that don't share a
//!   DB transaction with the resource table need an explicit signal to
//!   delete their tuples.

use std::sync::Arc;
use uuid::Uuid;

use sqlx::PgPool;

use crate::application::ports::authorization_ports::AuthorizationEngine;
use crate::common::errors::DomainError;
use crate::domain::services::authorization::{
    Grant, GrantCursor, IncomingGrantSummary, Permission, Resource, ResourceKind, Subject,
};
use crate::infrastructure::repositories::pg::file_blob_read_repository::FileBlobReadRepository;
use crate::infrastructure::repositories::pg::folder_db_repository::FolderDbRepository;

pub struct PgAclEngine {
    pool: Arc<PgPool>,
    folder_repo: Arc<FolderDbRepository>,
    file_repo: Arc<FileBlobReadRepository>,
}

impl PgAclEngine {
    pub fn new(
        pool: Arc<PgPool>,
        folder_repo: Arc<FolderDbRepository>,
        file_repo: Arc<FileBlobReadRepository>,
    ) -> Self {
        Self {
            pool,
            folder_repo,
            file_repo,
        }
    }

    /// Creates a stub instance for tests that need to construct services
    /// without a real PostgreSQL pool. Connecting to the lazy pool will
    /// fail at runtime — only safe in tests that exercise types, not actual
    /// authz queries.
    #[cfg(test)]
    pub fn new_stub() -> Self {
        let pool = sqlx::pool::PoolOptions::<sqlx::Postgres>::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid:5432/none")
            .unwrap();
        Self {
            pool: Arc::new(pool),
            folder_repo: Arc::new(FolderDbRepository::new_stub()),
            file_repo: Arc::new(FileBlobReadRepository::new_stub()),
        }
    }

    /// Returns the owner UUID for any resource type.
    async fn owner_of(&self, resource: Resource) -> Result<Uuid, DomainError> {
        match resource {
            Resource::Folder(id) => self.folder_repo.get_folder_user_id(&id.to_string()).await,
            Resource::File(id) => self.file_repo.get_file_user_id(&id.to_string()).await,
        }
    }

    /// Cascading check for folders: is there a grant on any ancestor folder
    /// (including the target itself) in this subject + permission?
    /// Uses GiST index on `storage.folders.lpath`.
    async fn folder_cascade_grant_exists(
        &self,
        subject: Subject,
        permission: Permission,
        folder_id: Uuid,
    ) -> Result<bool, DomainError> {
        let exists: Option<i32> = sqlx::query_scalar(
            r#"
            SELECT 1
              FROM storage.access_grants g
              JOIN storage.folders gf ON gf.id = g.resource_id
             WHERE g.subject_type  = $1
               AND g.subject_id    = $2
               AND g.permission    = $3
               AND g.resource_type = 'folder'
               AND (g.expires_at IS NULL OR g.expires_at > NOW())
               AND gf.lpath @> (SELECT lpath FROM storage.folders WHERE id = $4)
             LIMIT 1
            "#,
        )
        .bind(subject.type_str())
        .bind(subject.id())
        .bind(permission.as_str())
        .bind(folder_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("folder cascade: {e}")))?;

        Ok(exists.is_some())
    }

    /// Cascading check for files: either a direct file grant OR a grant on
    /// any ancestor folder of the file's containing folder.
    async fn file_cascade_grant_exists(
        &self,
        subject: Subject,
        permission: Permission,
        file_id: Uuid,
    ) -> Result<bool, DomainError> {
        let exists: Option<i32> = sqlx::query_scalar(
            r#"
            SELECT 1
              FROM (
                -- direct file grant
                SELECT 1
                  FROM storage.access_grants
                 WHERE subject_type = $1 AND subject_id = $2 AND permission = $3
                   AND resource_type = 'file' AND resource_id = $4
                   AND (expires_at IS NULL OR expires_at > NOW())
                UNION ALL
                -- cascading from any ancestor folder of the file's containing folder
                SELECT 1
                  FROM storage.access_grants g
                  JOIN storage.folders gf     ON gf.id = g.resource_id
                  JOIN storage.files target_f ON target_f.id = $4
                 WHERE g.subject_type  = $1
                   AND g.subject_id    = $2
                   AND g.permission    = $3
                   AND g.resource_type = 'folder'
                   AND (g.expires_at IS NULL OR g.expires_at > NOW())
                   AND target_f.folder_id IS NOT NULL
                   AND gf.lpath @> (SELECT lpath FROM storage.folders
                                     WHERE id = target_f.folder_id)
              ) any_match
             LIMIT 1
            "#,
        )
        .bind(subject.type_str())
        .bind(subject.id())
        .bind(permission.as_str())
        .bind(file_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("file cascade: {e}")))?;

        Ok(exists.is_some())
    }

    /// Look up a single grant by id. Returns `(resource, granted_by)` so
    /// the REST `DELETE /api/grants/{id}` handler can decide authorization
    /// without a second round-trip. Returns `Ok(None)` if no such grant.
    pub async fn find_grant_by_id(
        &self,
        grant_id: Uuid,
    ) -> Result<Option<(Resource, Uuid)>, DomainError> {
        let row: Option<(String, Uuid, Uuid)> = sqlx::query_as(
            "SELECT resource_type, resource_id, granted_by FROM storage.access_grants WHERE id = $1",
        )
        .bind(grant_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("find_grant_by_id: {e}")))?;

        let Some((rt, rid, granter)) = row else {
            return Ok(None);
        };
        let res = Resource::from_parts(&rt, rid)
            .ok_or_else(|| DomainError::internal_error("PgAcl", "unknown resource_type"))?;
        Ok(Some((res, granter)))
    }

    /// Row type for all full-grant SELECT queries:
    /// (id, subject_type, subject_id, resource_type, resource_id, permission, granted_by, granted_at, expires_at)
    #[allow(clippy::type_complexity)]
    fn row_to_grant(
        row: (
            Uuid,
            String,
            Uuid,
            String,
            Uuid,
            String,
            Uuid,
            chrono::DateTime<chrono::Utc>,
            Option<chrono::DateTime<chrono::Utc>>,
        ),
    ) -> Result<Grant, DomainError> {
        let subject = Subject::from_parts(&row.1, row.2)
            .ok_or_else(|| DomainError::internal_error("PgAcl", "unknown subject_type"))?;
        let resource = Resource::from_parts(&row.3, row.4)
            .ok_or_else(|| DomainError::internal_error("PgAcl", "unknown resource_type"))?;
        let permission = Permission::parse(&row.5)
            .ok_or_else(|| DomainError::internal_error("PgAcl", "unknown permission"))?;
        Ok(Grant {
            id: row.0,
            subject,
            resource,
            permission,
            granted_by: row.6,
            granted_at: row.7,
            expires_at: row.8,
        })
    }
}

impl AuthorizationEngine for PgAclEngine {
    async fn check(
        &self,
        subject: Subject,
        permission: Permission,
        resource: Resource,
    ) -> Result<bool, DomainError> {
        // Owner short-circuit (only for User subjects — groups/tokens/external
        // are never owners of resources).
        if let Subject::User(uid) = subject {
            match self.owner_of(resource).await {
                Ok(owner) if owner == uid => return Ok(true),
                Ok(_) => { /* not owner — fall through to grants */ }
                Err(e) if e.kind == crate::common::errors::ErrorKind::NotFound => {
                    // Resource doesn't exist — no permission. Return false
                    // rather than propagating NotFound; the caller (`require`)
                    // converts a false back to NotFound on its own.
                    return Ok(false);
                }
                Err(e) => return Err(e),
            }
        }

        // Cascading grant check.
        match resource {
            Resource::Folder(id) => {
                self.folder_cascade_grant_exists(subject, permission, id)
                    .await
            }
            Resource::File(id) => {
                self.file_cascade_grant_exists(subject, permission, id)
                    .await
            }
        }
    }

    async fn list_incoming_grants(
        &self,
        subject: Subject,
        permission_filter: Option<Permission>,
    ) -> Result<Vec<Grant>, DomainError> {
        let perm_str = permission_filter.map(|p| p.as_str().to_string());

        let rows = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Uuid,
                String,
                Uuid,
                String,
                Uuid,
                chrono::DateTime<chrono::Utc>,
                Option<chrono::DateTime<chrono::Utc>>,
            ),
        >(
            r#"
            SELECT id, subject_type, subject_id, resource_type, resource_id,
                   permission, granted_by, granted_at, expires_at
              FROM storage.access_grants
             WHERE subject_type = $1
               AND subject_id   = $2
               AND ($3::text IS NULL OR permission = $3)
             ORDER BY granted_at DESC
            "#,
        )
        .bind(subject.type_str())
        .bind(subject.id())
        .bind(perm_str)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("list incoming: {e}")))?;

        rows.into_iter().map(Self::row_to_grant).collect()
    }

    async fn list_incoming_resources_paged(
        &self,
        subject: Subject,
        kinds: &[ResourceKind],
        limit: u32,
        cursor: Option<GrantCursor>,
        sort_by: &str,
        reverse: bool,
    ) -> Result<(Vec<IncomingGrantSummary>, Option<GrantCursor>), DomainError> {
        // ── Common setup ──────────────────────────────────────────────────────
        let kind_strs: Option<Vec<&str>> = if kinds.is_empty() {
            None
        } else {
            Some(kinds.iter().map(|k| k.as_str()).collect())
        };
        let fetch_limit = (limit as i64) + 1;

        // Unified row type — the last two columns carry the sort key when present,
        // NULL otherwise.  This lets every sort mode share a single query_as call.
        //   0 resource_type  String
        //   1 resource_id    Uuid
        //   2 permissions    Vec<String>
        //   3 granted_at     DateTime<Utc>
        //   4 granted_by     Uuid
        //   5 sort_str       Option<String>  — resource_name (name/type) or owner_name (granted_by)
        //   6 sort_int       Option<i64>     — category_order (type) or file size in bytes (size)
        type Row = (
            String,
            Uuid,
            Vec<String>,
            chrono::DateTime<chrono::Utc>,
            Uuid,
            Option<String>,
            Option<i64>,
        );

        // Extract all cursor fields up-front; each branch uses the subset it needs.
        // Fixed parameter positions used in all SQL variants:
        //   $4 = cursor_str  (resource_name / owner_name)
        //   $5 = cursor_int  (type_order)
        //   $6 = cursor_at   (granted_at)
        //   $7 = cursor_id   (resource_id)
        //   $8 = fetch_limit
        let cursor_str = cursor.as_ref().and_then(|c| c.resource_name.clone());
        let cursor_int = cursor.as_ref().and_then(|c| c.sort_int);
        let cursor_at = cursor.as_ref().map(|c| c.granted_at);
        let cursor_id = cursor.as_ref().map(|c| c.resource_id);

        // ── agg CTE (identical in all branches) ───────────────────────────────
        const AGG: &str = r#"agg AS (
            SELECT
                resource_type,
                resource_id,
                array_agg(DISTINCT permission ORDER BY permission) AS permissions,
                MIN(granted_at)                                    AS granted_at,
                (array_agg(granted_by ORDER BY granted_at))[1]    AS granted_by
            FROM storage.access_grants
            WHERE subject_type = $1
              AND subject_id   = $2
              AND ($3::text[] IS NULL OR resource_type = ANY($3))
            GROUP BY resource_type, resource_id
        )"#;

        // ── Build sort-specific SQL fragments ─────────────────────────────────
        // "name" and "type" share the same LEFT JOINs; only sort_int_expr,
        // the cursor WHERE condition, and ORDER BY differ.
        // Each branch emits two variants selected by `reverse`.
        let sql = match sort_by {
            "name" | "type" => {
                let sort_int_expr = if sort_by == "type" {
                    "CASE WHEN agg.resource_type = 'folder' THEN 0 ELSE fi.category_order::bigint END"
                } else {
                    "NULL::bigint"
                };
                // Normal vs reversed keyset + ORDER BY.
                let (where_clause, order_clause) = if sort_by == "type" {
                    if reverse {
                        (
                            r#"(  $5::integer IS NULL
                               OR sort_int < $5
                               OR (sort_int = $5 AND LOWER(sort_str) < $4)
                               OR (sort_int = $5 AND LOWER(sort_str) = $4 AND resource_id < $7::uuid))"#,
                            "sort_int DESC, LOWER(sort_str) DESC, resource_id DESC",
                        )
                    } else {
                        (
                            r#"(  $5::integer IS NULL
                               OR sort_int > $5
                               OR (sort_int = $5 AND LOWER(sort_str) > $4)
                               OR (sort_int = $5 AND LOWER(sort_str) = $4 AND resource_id > $7::uuid))"#,
                            "sort_int ASC, LOWER(sort_str) ASC, resource_id ASC",
                        )
                    }
                } else if reverse {
                    (
                        r#"(  $4::text IS NULL
                           OR LOWER(sort_str) < $4
                           OR (LOWER(sort_str) = $4 AND resource_id < $7::uuid))"#,
                        "LOWER(sort_str) DESC, resource_id DESC",
                    )
                } else {
                    (
                        r#"(  $4::text IS NULL
                           OR LOWER(sort_str) > $4
                           OR (LOWER(sort_str) = $4 AND resource_id > $7::uuid))"#,
                        "LOWER(sort_str) ASC, resource_id ASC",
                    )
                };
                format!(
                    r#"WITH {AGG},
                    named AS (
                        SELECT agg.*,
                            COALESCE(
                                CASE WHEN agg.resource_type = 'folder' THEN f.name  END,
                                CASE WHEN agg.resource_type = 'file'   THEN fi.name END
                            ) AS sort_str,
                            {sort_int_expr} AS sort_int
                        FROM agg
                        LEFT JOIN storage.folders f  ON f.id  = agg.resource_id AND agg.resource_type = 'folder'
                        LEFT JOIN storage.files   fi ON fi.id = agg.resource_id AND agg.resource_type = 'file'
                    )
                    SELECT resource_type, resource_id, permissions, granted_at, granted_by, sort_str, sort_int
                    FROM named
                    WHERE {where_clause}
                    ORDER BY {order_clause}
                    LIMIT $8"#
                )
            }
            "granted_by" => {
                // Joins auth.users to sort alphabetically by username.
                // Cursor encodes (owner_name=$4, granted_at=$6, resource_id=$7).
                let (where_clause, order_clause) = if reverse {
                    (
                        r#"(  $4::text IS NULL
                          OR sort_str < $4
                          OR (sort_str = $4 AND (
                                  $6::timestamptz IS NULL
                               OR granted_at > $6
                               OR (granted_at = $6 AND resource_id > $7::uuid))))"#,
                        "sort_str DESC, granted_at ASC, resource_id ASC",
                    )
                } else {
                    (
                        r#"(  $4::text IS NULL
                          OR sort_str > $4
                          OR (sort_str = $4 AND (
                                  $6::timestamptz IS NULL
                               OR granted_at < $6
                               OR (granted_at = $6 AND resource_id < $7::uuid))))"#,
                        "sort_str ASC, granted_at DESC, resource_id DESC",
                    )
                };
                format!(
                    r#"WITH {AGG},
                    owner_named AS (
                        SELECT agg.*,
                            LOWER(u.username) AS sort_str,
                            NULL::bigint AS sort_int
                        FROM agg
                        LEFT JOIN auth.users u ON u.id = agg.granted_by
                    )
                    SELECT resource_type, resource_id, permissions, granted_at, granted_by, sort_str, sort_int
                    FROM owner_named
                    WHERE {where_clause}
                    ORDER BY {order_clause}
                    LIMIT $8"#
                )
            }
            "size" => {
                // Folders have no size — sentinel -1 (sorts first ASC, last DESC).
                // Cursor encodes (sort_int=$5, resource_id=$7); $4/$6 unused.
                let (where_clause, order_clause) = if reverse {
                    (
                        r#"(  $5::bigint IS NULL
                          OR sort_int < $5
                          OR (sort_int = $5 AND resource_id < $7::uuid))"#,
                        "sort_int DESC, resource_id DESC",
                    )
                } else {
                    (
                        r#"(  $5::bigint IS NULL
                          OR sort_int > $5
                          OR (sort_int = $5 AND resource_id > $7::uuid))"#,
                        "sort_int ASC, resource_id ASC",
                    )
                };
                format!(
                    r#"WITH {AGG},
                    sized AS (
                        SELECT agg.*,
                            NULL::text AS sort_str,
                            CASE WHEN agg.resource_type = 'folder' THEN -1
                                 ELSE fi.size
                            END AS sort_int
                        FROM agg
                        LEFT JOIN storage.files fi ON fi.id = agg.resource_id AND agg.resource_type = 'file'
                    )
                    SELECT resource_type, resource_id, permissions, granted_at, granted_by, sort_str, sort_int
                    FROM sized
                    WHERE {where_clause}
                    ORDER BY {order_clause}
                    LIMIT $8"#
                )
            }
            _ => {
                // Default: sort by grant date.
                // Normal = DESC (newest first); reversed = ASC (oldest first).
                // Cursor encodes (granted_at=$6, resource_id=$7); $4/$5 unused.
                let (where_clause, order_clause) = if reverse {
                    (
                        r#"(  $6::timestamptz IS NULL
                          OR granted_at > $6
                          OR (granted_at = $6 AND resource_id > $7::uuid))"#,
                        "granted_at ASC, resource_id ASC",
                    )
                } else {
                    (
                        r#"(  $6::timestamptz IS NULL
                          OR granted_at < $6
                          OR (granted_at = $6 AND resource_id < $7::uuid))"#,
                        "granted_at DESC, resource_id DESC",
                    )
                };
                format!(
                    r#"WITH {AGG}
                    SELECT resource_type, resource_id, permissions, granted_at, granted_by,
                           NULL::text   AS sort_str,
                           NULL::bigint AS sort_int
                    FROM agg
                    WHERE {where_clause}
                    ORDER BY {order_clause}
                    LIMIT $8"#
                )
            }
        };

        // ── Execute — uniform 8 binds for every sort mode ─────────────────────
        let mut rows: Vec<Row> = sqlx::query_as::<_, Row>(&sql)
            .bind(subject.type_str()) // $1
            .bind(subject.id()) // $2
            .bind(&kind_strs) // $3
            .bind(&cursor_str) // $4 sort_str cursor
            .bind(cursor_int) // $5 sort_int cursor
            .bind(cursor_at) // $6 granted_at cursor
            .bind(cursor_id) // $7 resource_id cursor
            .bind(fetch_limit) // $8
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| {
                DomainError::internal_error(
                    "PgAcl",
                    format!("list_incoming_resources_paged ({sort_by}): {e}"),
                )
            })?;

        // ── Pagination ────────────────────────────────────────────────────────
        let has_next = rows.len() > limit as usize;
        rows.truncate(limit as usize);

        let next_cursor = if has_next {
            rows.last().map(|r| {
                let sort_str_lc = r.5.as_deref().map(str::to_lowercase);
                match sort_by {
                    "name" => GrantCursor {
                        sort_by: "name".to_owned(),
                        granted_at: r.3,
                        resource_id: r.1,
                        resource_name: sort_str_lc,
                        sort_int: None,
                        reverse,
                    },
                    "type" => GrantCursor {
                        sort_by: "type".to_owned(),
                        granted_at: r.3,
                        resource_id: r.1,
                        resource_name: sort_str_lc,
                        sort_int: r.6,
                        reverse,
                    },
                    "granted_by" => GrantCursor {
                        sort_by: "granted_by".to_owned(),
                        granted_at: r.3,
                        resource_id: r.1,
                        resource_name: r.5.clone(), // already lowercased by SQL
                        sort_int: None,
                        reverse,
                    },
                    "size" => GrantCursor {
                        sort_by: "size".to_owned(),
                        granted_at: r.3,
                        resource_id: r.1,
                        resource_name: None,
                        sort_int: r.6,
                        reverse,
                    },
                    _ => GrantCursor {
                        sort_by: "granted_at".to_owned(),
                        granted_at: r.3,
                        resource_id: r.1,
                        resource_name: None,
                        sort_int: None,
                        reverse,
                    },
                }
            })
        } else {
            None
        };

        // ── Convert rows to domain summaries ──────────────────────────────────
        let summaries = rows
            .into_iter()
            .filter_map(|(rt, rid, perms_str, granted_at, granted_by, _, _)| {
                let resource_type = ResourceKind::parse(&rt)?;
                let permissions = perms_str
                    .into_iter()
                    .filter_map(|s| Permission::parse(&s))
                    .collect();
                Some(IncomingGrantSummary {
                    resource_type,
                    resource_id: rid,
                    permissions,
                    granted_at,
                    granted_by,
                })
            })
            .collect();

        Ok((summaries, next_cursor))
    }

    async fn list_grants_on_resource(&self, resource: Resource) -> Result<Vec<Grant>, DomainError> {
        let rows = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Uuid,
                String,
                Uuid,
                String,
                Uuid,
                chrono::DateTime<chrono::Utc>,
                Option<chrono::DateTime<chrono::Utc>>,
            ),
        >(
            r#"
            SELECT id, subject_type, subject_id, resource_type, resource_id,
                   permission, granted_by, granted_at, expires_at
              FROM storage.access_grants
             WHERE resource_type = $1
               AND resource_id   = $2
             ORDER BY granted_at DESC
            "#,
        )
        .bind(resource.type_str())
        .bind(resource.id())
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("list on resource: {e}")))?;

        rows.into_iter().map(Self::row_to_grant).collect()
    }

    async fn list_outgoing_grants(&self, granted_by: Uuid) -> Result<Vec<Grant>, DomainError> {
        let rows = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Uuid,
                String,
                Uuid,
                String,
                Uuid,
                chrono::DateTime<chrono::Utc>,
                Option<chrono::DateTime<chrono::Utc>>,
            ),
        >(
            r#"
            SELECT id, subject_type, subject_id, resource_type, resource_id,
                   permission, granted_by, granted_at, expires_at
              FROM storage.access_grants
             WHERE granted_by = $1
             ORDER BY granted_at DESC
            "#,
        )
        .bind(granted_by)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("list outgoing: {e}")))?;

        rows.into_iter().map(Self::row_to_grant).collect()
    }

    async fn grant(
        &self,
        granted_by: Uuid,
        subject: Subject,
        permission: Permission,
        resource: Resource,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Grant, DomainError> {
        let row = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Uuid,
                String,
                Uuid,
                String,
                Uuid,
                chrono::DateTime<chrono::Utc>,
                Option<chrono::DateTime<chrono::Utc>>,
            ),
        >(
            r#"
            INSERT INTO storage.access_grants
                (subject_type, subject_id, resource_type, resource_id, permission, granted_by, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (subject_type, subject_id, resource_type, resource_id, permission)
            DO UPDATE SET expires_at = EXCLUDED.expires_at
            RETURNING id, subject_type, subject_id, resource_type, resource_id,
                      permission, granted_by, granted_at, expires_at
            "#,
        )
        .bind(subject.type_str())
        .bind(subject.id())
        .bind(resource.type_str())
        .bind(resource.id())
        .bind(permission.as_str())
        .bind(granted_by)
        .bind(expires_at)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("insert grant: {e}")))?;

        Self::row_to_grant(row)
    }

    async fn set_expiry_for_subject(
        &self,
        subject: Subject,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), DomainError> {
        sqlx::query(
            "UPDATE storage.access_grants SET expires_at = $3 WHERE subject_type = $1 AND subject_id = $2",
        )
        .bind(subject.type_str())
        .bind(subject.id())
        .bind(expires_at)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("set_expiry_for_subject: {e}")))?;
        Ok(())
    }

    async fn set_expiry_on_resource(
        &self,
        subject: Subject,
        resource: Resource,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), DomainError> {
        sqlx::query(
            "UPDATE storage.access_grants SET expires_at = $3 \
             WHERE subject_type = $1 AND subject_id = $2 \
             AND resource_type = $4 AND resource_id = $5",
        )
        .bind(subject.type_str())
        .bind(subject.id())
        .bind(expires_at)
        .bind(resource.type_str())
        .bind(resource.id())
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("set_expiry_on_resource: {e}")))?;
        Ok(())
    }

    async fn revoke(&self, grant_id: Uuid) -> Result<(), DomainError> {
        sqlx::query("DELETE FROM storage.access_grants WHERE id = $1")
            .bind(grant_id)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| DomainError::internal_error("PgAcl", format!("revoke: {e}")))?;
        Ok(())
    }

    async fn revoke_all_for_resource(&self, resource: Resource) -> Result<usize, DomainError> {
        let result = sqlx::query(
            "DELETE FROM storage.access_grants WHERE resource_type = $1 AND resource_id = $2",
        )
        .bind(resource.type_str())
        .bind(resource.id())
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("revoke for resource: {e}")))?;

        Ok(result.rows_affected() as usize)
    }

    async fn revoke_all_for_subject(&self, subject: Subject) -> Result<usize, DomainError> {
        let result = sqlx::query(
            "DELETE FROM storage.access_grants WHERE subject_type = $1 AND subject_id = $2",
        )
        .bind(subject.type_str())
        .bind(subject.id())
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("PgAcl", format!("revoke for subject: {e}")))?;

        Ok(result.rows_affected() as usize)
    }
}
