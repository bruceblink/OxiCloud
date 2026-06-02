//! PostgreSQL implementation of [`MagicLinkTokenRepository`].
//!
//! Mirrors the layout of `device_code_pg_repository.rs` — same crate
//! conventions (handcrafted SQL, `Row` extraction in a `map_row` helper,
//! enum cast in the INSERT statement).

use async_trait::async_trait;
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::sync::Arc;
use uuid::Uuid;

use crate::common::errors::{DomainError, ErrorKind};
use crate::domain::entities::magic_link_token::{
    MagicLinkResourceKind, MagicLinkStatus, MagicLinkToken,
};
use crate::domain::repositories::magic_link_token_repository::MagicLinkTokenRepository;

pub struct MagicLinkTokenPgRepository {
    pool: Arc<PgPool>,
}

impl MagicLinkTokenPgRepository {
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    fn map_row(row: &sqlx::postgres::PgRow) -> Result<MagicLinkToken, DomainError> {
        let status_str: String = row.try_get("status").map_err(|e| {
            DomainError::new(
                ErrorKind::DatabaseError,
                "MagicLinkToken",
                format!("read status: {}", e),
            )
        })?;
        let status = MagicLinkStatus::parse(&status_str).unwrap_or(MagicLinkStatus::Expired);

        let resource_type: Option<String> = row.try_get("resource_type").ok();
        let resource_kind = resource_type.and_then(|s| MagicLinkResourceKind::parse(&s));
        let resource_id: Option<Uuid> = row.try_get("resource_id").ok();
        let request_challenge: Option<String> = row.try_get("request_challenge").ok();

        Ok(MagicLinkToken::from_raw(
            row.try_get("id").unwrap(),
            row.try_get("token").unwrap_or_default(),
            row.try_get("user_id").unwrap(),
            status,
            row.try_get("issued_at").unwrap_or_default(),
            row.try_get("expires_at").unwrap_or_default(),
            row.try_get("used_at").ok(),
            resource_kind,
            resource_id,
            request_challenge,
        ))
    }
}

#[async_trait]
impl MagicLinkTokenRepository for MagicLinkTokenPgRepository {
    async fn create(&self, token: &MagicLinkToken) -> Result<(), DomainError> {
        sqlx::query(
            r#"
            INSERT INTO auth.magic_link_tokens (
                id, token, user_id, status,
                issued_at, expires_at, used_at,
                resource_type, resource_id,
                request_challenge
            ) VALUES (
                $1, $2, $3, $4::auth.magic_link_status,
                $5, $6, $7,
                $8, $9,
                $10
            )
            "#,
        )
        .bind(token.id())
        .bind(token.token())
        .bind(token.user_id())
        .bind(token.status().as_str())
        .bind(token.issued_at())
        .bind(token.expires_at())
        .bind(token.used_at())
        .bind(token.resource_kind().map(|k| k.as_str()))
        .bind(token.resource_id())
        .bind(token.request_challenge())
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("MagicLinkToken", format!("insert: {}", e)))?;
        Ok(())
    }

    async fn find_by_token(&self, token: &str) -> Result<Option<MagicLinkToken>, DomainError> {
        let row = sqlx::query(
            r#"
            SELECT id, token, user_id, status::text AS status,
                   issued_at, expires_at, used_at,
                   resource_type, resource_id,
                   request_challenge
              FROM auth.magic_link_tokens
             WHERE token = $1
            "#,
        )
        .bind(token)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| {
            DomainError::internal_error("MagicLinkToken", format!("find_by_token: {}", e))
        })?;

        row.map(|r| Self::map_row(&r)).transpose()
    }

    async fn mark_used(&self, id: Uuid) -> Result<bool, DomainError> {
        // The `status = 'pending'` predicate is what makes single-use
        // race-free: a concurrent redemption attempt sees the row
        // already updated (or in the middle of being updated, blocking
        // on Postgres' row lock) and gets `rows_affected = 0`.
        let result = sqlx::query(
            r#"
            UPDATE auth.magic_link_tokens
               SET status  = 'used'::auth.magic_link_status,
                   used_at = NOW()
             WHERE id      = $1
               AND status  = 'pending'::auth.magic_link_status
               AND expires_at > NOW()
            "#,
        )
        .bind(id)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DomainError::internal_error("MagicLinkToken", format!("mark_used: {}", e)))?;

        Ok(result.rows_affected() == 1)
    }

    async fn delete_expired(&self) -> Result<u64, DomainError> {
        // Hard-delete: the audit trail lives in the `tracing` log, not
        // the table. Keeping expired rows around would just bloat the
        // index without adding security value.
        let result = sqlx::query(
            r#"
            DELETE FROM auth.magic_link_tokens
             WHERE status = 'pending'::auth.magic_link_status
               AND expires_at < NOW()
            "#,
        )
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| {
            DomainError::internal_error("MagicLinkToken", format!("delete_expired: {}", e))
        })?;

        Ok(result.rows_affected())
    }

    async fn delete_all_for_user_tx(
        &self,
        user_id: Uuid,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<u64, DomainError> {
        let result = sqlx::query(
            r#"
            DELETE FROM auth.magic_link_tokens
             WHERE user_id = $1
            "#,
        )
        .bind(user_id)
        .execute(&mut **tx)
        .await
        .map_err(|e| {
            DomainError::internal_error("MagicLinkToken", format!("delete_all_for_user_tx: {}", e))
        })?;

        Ok(result.rows_affected())
    }
}
