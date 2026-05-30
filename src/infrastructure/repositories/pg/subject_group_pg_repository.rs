//! Postgres implementation of `SubjectGroupRepository`.
//!
//! Two queries are non-trivial and deserve a read pass:
//!   - **Cycle check** (write-time, inside `add_member` when adding a
//!     group-member): walks child-edges from the candidate; if the parent
//!     appears in the descendants, reject.
//!   - **Transitive expansion** (`groups_for_user`): hot path on every
//!     authz cache miss; walks parent-edges from the user's direct
//!     memberships upward through nested groups.
//!
//! Depth-cap (`MAX_GROUP_DEPTH = 8`) is enforced at write time inside the
//! same transaction as the membership insert.
//!
//! See `migrations/20260612000000_subject_groups.sql` for the schema.

use std::collections::HashSet;
use std::sync::Arc;

use sqlx::{PgPool, Row, types::Uuid};

use super::like_escape;
use crate::domain::entities::subject_group::{GroupMember, MAX_GROUP_DEPTH, SubjectGroup};
use crate::domain::repositories::subject_group_repository::{
    SubjectGroupRepository, SubjectGroupRepositoryError,
};

pub struct SubjectGroupPgRepository {
    pool: Arc<PgPool>,
}

impl SubjectGroupPgRepository {
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    fn map_sqlx_err(context: &'static str, e: sqlx::Error) -> SubjectGroupRepositoryError {
        // Recognise common Postgres errors and translate to typed variants.
        if let sqlx::Error::Database(ref dberr) = e
            && let Some(code) = dberr.code()
        {
            match code.as_ref() {
                // unique_violation — name collision (or duplicate member, but
                // the caller already handles that case via UNIQUE indexes
                // returning the same code).
                "23505" => {
                    return SubjectGroupRepositoryError::NameAlreadyExists(dberr.to_string());
                }
                // check_violation — RFC 5321 regex CHECK failed.
                "23514" => return SubjectGroupRepositoryError::InvalidName(dberr.to_string()),
                _ => {}
            }
        }
        SubjectGroupRepositoryError::StorageError(format!("{}: {}", context, e))
    }

    fn row_to_group(row: &sqlx::postgres::PgRow) -> SubjectGroup {
        SubjectGroup {
            id: row.get::<Uuid, _>("id"),
            name: row.get::<String, _>("name"),
            description: row.get::<Option<String>, _>("description"),
            is_virtual: row.get::<bool, _>("is_virtual"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        }
    }
}

impl SubjectGroupRepository for SubjectGroupPgRepository {
    async fn create(
        &self,
        group: &SubjectGroup,
    ) -> Result<SubjectGroup, SubjectGroupRepositoryError> {
        let row = sqlx::query(
            "INSERT INTO auth.subject_groups (id, name, description, is_virtual, created_at, updated_at)
             VALUES ($1, $2, $3, false, $4, $5)
             RETURNING id, name, description, is_virtual, created_at, updated_at",
        )
        .bind(group.id)
        .bind(&group.name)
        .bind(&group.description)
        .bind(group.created_at)
        .bind(group.updated_at)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| Self::map_sqlx_err("create subject_group", e))?;

        Ok(Self::row_to_group(&row))
    }

    async fn get_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<SubjectGroup>, SubjectGroupRepositoryError> {
        let row = sqlx::query(
            "SELECT id, name, description, is_virtual, created_at, updated_at
             FROM auth.subject_groups WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| Self::map_sqlx_err("get_by_id", e))?;

        Ok(row.as_ref().map(Self::row_to_group))
    }

    async fn get_by_name(
        &self,
        name: &str,
    ) -> Result<Option<SubjectGroup>, SubjectGroupRepositoryError> {
        // CITEXT matches case-insensitively — no need for LOWER() here.
        let row = sqlx::query(
            "SELECT id, name, description, is_virtual, created_at, updated_at
             FROM auth.subject_groups WHERE name = $1",
        )
        .bind(name)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| Self::map_sqlx_err("get_by_name", e))?;

        Ok(row.as_ref().map(Self::row_to_group))
    }

    async fn list(
        &self,
        limit: u32,
        offset: u32,
        name_query: Option<&str>,
    ) -> Result<(Vec<SubjectGroup>, u64), SubjectGroupRepositoryError> {
        // Two queries: one for the page, one for the total count. The query
        // is small and frequent; a window function would add complexity for
        // no measurable win.
        let (sql_page, sql_count, pattern) = match name_query {
            Some(q) => {
                let pat = like_escape(q);
                (
                    "SELECT id, name, description, is_virtual, created_at, updated_at
                     FROM auth.subject_groups
                     WHERE name ILIKE $1
                     ORDER BY is_virtual DESC, name
                     LIMIT $2 OFFSET $3"
                        .to_string(),
                    "SELECT COUNT(*) FROM auth.subject_groups WHERE name ILIKE $1".to_string(),
                    Some(pat),
                )
            }
            None => (
                "SELECT id, name, description, is_virtual, created_at, updated_at
                 FROM auth.subject_groups
                 ORDER BY is_virtual DESC, name
                 LIMIT $1 OFFSET $2"
                    .to_string(),
                "SELECT COUNT(*) FROM auth.subject_groups".to_string(),
                None,
            ),
        };

        let rows = if let Some(ref p) = pattern {
            sqlx::query(&sql_page)
                .bind(p)
                .bind(limit as i64)
                .bind(offset as i64)
                .fetch_all(self.pool.as_ref())
                .await
        } else {
            sqlx::query(&sql_page)
                .bind(limit as i64)
                .bind(offset as i64)
                .fetch_all(self.pool.as_ref())
                .await
        }
        .map_err(|e| Self::map_sqlx_err("list page", e))?;

        let total: i64 = if let Some(ref p) = pattern {
            sqlx::query_scalar(&sql_count)
                .bind(p)
                .fetch_one(self.pool.as_ref())
                .await
        } else {
            sqlx::query_scalar(&sql_count)
                .fetch_one(self.pool.as_ref())
                .await
        }
        .map_err(|e| Self::map_sqlx_err("list count", e))?;

        Ok((rows.iter().map(Self::row_to_group).collect(), total as u64))
    }

    async fn list_with_counts(
        &self,
        limit: u32,
        offset: u32,
        name_query: Option<&str>,
    ) -> Result<(Vec<(SubjectGroup, i64)>, u64), SubjectGroupRepositoryError> {
        // Single SQL: groups + COUNT of direct members per group, via LEFT JOIN
        // on `auth.subject_group_members`. No N+1; one round-trip for the
        // page, a second for the unfiltered total (matches `list`).
        let (sql_page, sql_count, pattern) = match name_query {
            Some(q) => {
                let pat = like_escape(q);
                (
                    "SELECT g.id, g.name, g.description, g.is_virtual,
                            g.created_at, g.updated_at,
                            COUNT(m.group_id) AS member_count
                     FROM auth.subject_groups g
                     LEFT JOIN auth.subject_group_members m ON m.group_id = g.id
                     WHERE g.name ILIKE $1
                     GROUP BY g.id
                     ORDER BY g.is_virtual DESC, g.name
                     LIMIT $2 OFFSET $3"
                        .to_string(),
                    "SELECT COUNT(*) FROM auth.subject_groups WHERE name ILIKE $1".to_string(),
                    Some(pat),
                )
            }
            None => (
                "SELECT g.id, g.name, g.description, g.is_virtual,
                        g.created_at, g.updated_at,
                        COUNT(m.group_id) AS member_count
                 FROM auth.subject_groups g
                 LEFT JOIN auth.subject_group_members m ON m.group_id = g.id
                 GROUP BY g.id
                 ORDER BY g.is_virtual DESC, g.name
                 LIMIT $1 OFFSET $2"
                    .to_string(),
                "SELECT COUNT(*) FROM auth.subject_groups".to_string(),
                None,
            ),
        };

        let rows = if let Some(ref p) = pattern {
            sqlx::query(&sql_page)
                .bind(p)
                .bind(limit as i64)
                .bind(offset as i64)
                .fetch_all(self.pool.as_ref())
                .await
        } else {
            sqlx::query(&sql_page)
                .bind(limit as i64)
                .bind(offset as i64)
                .fetch_all(self.pool.as_ref())
                .await
        }
        .map_err(|e| Self::map_sqlx_err("list_with_counts page", e))?;

        let total: i64 = if let Some(ref p) = pattern {
            sqlx::query_scalar(&sql_count)
                .bind(p)
                .fetch_one(self.pool.as_ref())
                .await
        } else {
            sqlx::query_scalar(&sql_count)
                .fetch_one(self.pool.as_ref())
                .await
        }
        .map_err(|e| Self::map_sqlx_err("list_with_counts total", e))?;

        let items = rows
            .iter()
            .map(|r| (Self::row_to_group(r), r.get::<i64, _>("member_count")))
            .collect();

        Ok((items, total as u64))
    }

    async fn count_members(&self, id: Uuid) -> Result<i64, SubjectGroupRepositoryError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM auth.subject_group_members WHERE group_id = $1",
        )
        .bind(id)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| Self::map_sqlx_err("count_members", e))?;
        Ok(count)
    }

    async fn rename(
        &self,
        id: Uuid,
        new_name: &str,
    ) -> Result<SubjectGroup, SubjectGroupRepositoryError> {
        let row = sqlx::query(
            "UPDATE auth.subject_groups
             SET name = $2, updated_at = now()
             WHERE id = $1
             RETURNING id, name, description, is_virtual, created_at, updated_at",
        )
        .bind(id)
        .bind(new_name)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| Self::map_sqlx_err("rename", e))?;

        match row {
            Some(r) => Ok(Self::row_to_group(&r)),
            None => Err(SubjectGroupRepositoryError::NotFound(id.to_string())),
        }
    }

    async fn delete(&self, id: Uuid) -> Result<(), SubjectGroupRepositoryError> {
        // The application service is responsible for clearing related
        // `storage.access_grants` rows in the same transaction (there's no
        // FK between access_grants and subject_groups). The subject_group_members
        // rows cascade automatically via FK.
        let result = sqlx::query("DELETE FROM auth.subject_groups WHERE id = $1")
            .bind(id)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| Self::map_sqlx_err("delete", e))?;

        if result.rows_affected() == 0 {
            return Err(SubjectGroupRepositoryError::NotFound(id.to_string()));
        }
        Ok(())
    }

    async fn add_member(
        &self,
        group_id: Uuid,
        member: GroupMember,
        added_by: Uuid,
    ) -> Result<(), SubjectGroupRepositoryError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| Self::map_sqlx_err("add_member: begin tx", e))?;

        // Lock the parent row to prevent racing concurrent adds from each
        // squeezing under the cycle/depth limits.
        let exists: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM auth.subject_groups WHERE id = $1 FOR UPDATE")
                .bind(group_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| Self::map_sqlx_err("add_member: lock parent", e))?;
        if exists.is_none() {
            return Err(SubjectGroupRepositoryError::NotFound(group_id.to_string()));
        }

        match member {
            GroupMember::User(user_id) => {
                // Plain insert. Unique index catches duplicates.
                let res = sqlx::query(
                    "INSERT INTO auth.subject_group_members
                       (group_id, member_user_id, added_by)
                     VALUES ($1, $2, $3)
                     ON CONFLICT DO NOTHING",
                )
                .bind(group_id)
                .bind(user_id)
                .bind(added_by)
                .execute(&mut *tx)
                .await
                .map_err(|e| Self::map_sqlx_err("add_member: insert user", e))?;

                if res.rows_affected() == 0 {
                    return Err(SubjectGroupRepositoryError::MemberAlreadyPresent);
                }
            }
            GroupMember::Group(member_group_id) => {
                if member_group_id == group_id {
                    return Err(SubjectGroupRepositoryError::Cycle(
                        "group cannot contain itself".to_string(),
                    ));
                }

                // ── Cycle check ─────────────────────────────────────────
                // Adding member_group_id=$child to group_id=$parent creates
                // a cycle iff $parent is reachable by walking child-edges
                // from $child. Use a bounded recursion (UNION de-dups).
                let cycle: Option<(i32,)> = sqlx::query_as(
                    "WITH RECURSIVE descendants AS (
                         SELECT member_group_id AS g
                           FROM auth.subject_group_members
                          WHERE group_id = $1 AND member_group_id IS NOT NULL
                         UNION
                         SELECT m.member_group_id
                           FROM auth.subject_group_members m
                           JOIN descendants d ON m.group_id = d.g
                          WHERE m.member_group_id IS NOT NULL
                     )
                     SELECT 1 FROM descendants WHERE g = $2 LIMIT 1",
                )
                .bind(member_group_id)
                .bind(group_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| Self::map_sqlx_err("add_member: cycle check", e))?;
                if cycle.is_some() {
                    return Err(SubjectGroupRepositoryError::Cycle(format!(
                        "{} → {}",
                        group_id, member_group_id
                    )));
                }

                // ── Depth check ─────────────────────────────────────────
                // The longest path from $parent after the mutation =
                // max(longest path from existing descendants, 1 + longest
                // path under $child). Compute both with the same CTE,
                // pretending the new edge already exists.
                let depth: Option<(i32,)> = sqlx::query_as(
                    "WITH RECURSIVE path AS (
                         -- existing depth from this group downward
                         SELECT member_group_id AS g, 1 AS depth
                           FROM auth.subject_group_members
                          WHERE group_id = $1 AND member_group_id IS NOT NULL
                         UNION ALL
                         -- proposed new edge
                         SELECT $2::uuid AS g, 1 AS depth
                         UNION ALL
                         SELECT m.member_group_id, p.depth + 1
                           FROM auth.subject_group_members m
                           JOIN path p ON m.group_id = p.g
                          WHERE m.member_group_id IS NOT NULL
                     )
                     SELECT MAX(depth) FROM path",
                )
                .bind(group_id)
                .bind(member_group_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| Self::map_sqlx_err("add_member: depth check", e))?;

                let max_depth = depth.map(|d| d.0).unwrap_or(0);
                if (max_depth as u8) > MAX_GROUP_DEPTH {
                    return Err(SubjectGroupRepositoryError::DepthExceeded(format!(
                        "would reach depth {} (max {})",
                        max_depth, MAX_GROUP_DEPTH
                    )));
                }

                // ── Insert ──────────────────────────────────────────────
                let res = sqlx::query(
                    "INSERT INTO auth.subject_group_members
                       (group_id, member_group_id, added_by)
                     VALUES ($1, $2, $3)
                     ON CONFLICT DO NOTHING",
                )
                .bind(group_id)
                .bind(member_group_id)
                .bind(added_by)
                .execute(&mut *tx)
                .await
                .map_err(|e| Self::map_sqlx_err("add_member: insert group", e))?;

                if res.rows_affected() == 0 {
                    return Err(SubjectGroupRepositoryError::MemberAlreadyPresent);
                }
            }
        }

        tx.commit()
            .await
            .map_err(|e| Self::map_sqlx_err("add_member: commit", e))?;
        Ok(())
    }

    async fn remove_member(
        &self,
        group_id: Uuid,
        member: GroupMember,
    ) -> Result<(), SubjectGroupRepositoryError> {
        let res = match member {
            GroupMember::User(uid) => sqlx::query(
                "DELETE FROM auth.subject_group_members
                  WHERE group_id = $1 AND member_user_id = $2",
            )
            .bind(group_id)
            .bind(uid),
            GroupMember::Group(gid) => sqlx::query(
                "DELETE FROM auth.subject_group_members
                  WHERE group_id = $1 AND member_group_id = $2",
            )
            .bind(group_id)
            .bind(gid),
        }
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| Self::map_sqlx_err("remove_member", e))?;

        if res.rows_affected() == 0 {
            return Err(SubjectGroupRepositoryError::MemberNotPresent);
        }
        Ok(())
    }

    async fn list_direct_members(
        &self,
        group_id: Uuid,
    ) -> Result<Vec<GroupMember>, SubjectGroupRepositoryError> {
        let rows = sqlx::query(
            "SELECT member_user_id, member_group_id
               FROM auth.subject_group_members
              WHERE group_id = $1",
        )
        .bind(group_id)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| Self::map_sqlx_err("list_direct_members", e))?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let user_id: Option<Uuid> = row.get("member_user_id");
            let group_id: Option<Uuid> = row.get("member_group_id");
            match (user_id, group_id) {
                (Some(uid), None) => out.push(GroupMember::User(uid)),
                (None, Some(gid)) => out.push(GroupMember::Group(gid)),
                _ => {
                    // XOR check at the schema level guarantees we never hit
                    // this branch — log defensively if we do.
                    tracing::warn!(
                        "subject_group_members row violates XOR invariant (user={:?}, group={:?})",
                        user_id,
                        group_id
                    );
                }
            }
        }
        Ok(out)
    }

    async fn list_transitive_users(
        &self,
        group_id: Uuid,
    ) -> Result<Vec<Uuid>, SubjectGroupRepositoryError> {
        // Walk child-edges from `group_id` to find every user transitively
        // a member. Used by debug / audit endpoints.
        let rows = sqlx::query(
            "WITH RECURSIVE descendants AS (
                 SELECT $1::uuid AS g
                 UNION
                 SELECT m.member_group_id
                   FROM auth.subject_group_members m
                   JOIN descendants d ON m.group_id = d.g
                  WHERE m.member_group_id IS NOT NULL
             )
             SELECT DISTINCT m.member_user_id AS user_id
               FROM auth.subject_group_members m
               JOIN descendants d ON m.group_id = d.g
              WHERE m.member_user_id IS NOT NULL",
        )
        .bind(group_id)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| Self::map_sqlx_err("list_transitive_users", e))?;

        Ok(rows.iter().map(|r| r.get::<Uuid, _>("user_id")).collect())
    }

    async fn groups_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<HashSet<Uuid>, SubjectGroupRepositoryError> {
        // The hot path. PgAclEngine::expand_subject calls this on every
        // cache miss; result is memoised in the Moka cache for ~30s.
        let rows = sqlx::query(
            "WITH RECURSIVE user_groups AS (
                 SELECT group_id
                   FROM auth.subject_group_members
                  WHERE member_user_id = $1
                 UNION
                 SELECT m.group_id
                   FROM auth.subject_group_members m
                   JOIN user_groups ug ON m.member_group_id = ug.group_id
             )
             SELECT group_id FROM user_groups",
        )
        .bind(user_id)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| Self::map_sqlx_err("groups_for_user", e))?;

        Ok(rows.iter().map(|r| r.get::<Uuid, _>("group_id")).collect())
    }
}
