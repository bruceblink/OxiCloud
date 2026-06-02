//! External-identity service.
//!
//! Houses the lifecycle hook for grant-only external users — recipients
//! authenticating via magic-link, OIDC-only, or OCM federation rather than
//! a local password. PR 8 populates two of the four hook methods:
//!
//! | Event             | Today's action                                     |
//! |-------------------|----------------------------------------------------|
//! | `on_user_created` | Audit event when the new user is external          |
//! | `on_user_login`   | Audit event when the logging-in user is external   |
//! | `on_user_logout`  | `Ok(())` — provenance is connection-level          |
//! | `on_user_deleted` | Explicit cleanup of outstanding magic-link tokens  |
//!
//! The token cleanup on deletion is technically redundant with the
//! `ON DELETE CASCADE` FK on `auth.magic_link_tokens.user_id`, but
//! calling it explicitly lets us:
//!   - Emit a single audit event with the row count.
//!   - Run inside the same transaction as the user DELETE so a hook
//!     failure aborts the whole thing (matches the `on_user_deleted`
//!     contract — see `user_lifecycle.rs` tip #7).
//!
//! # Future work
//!
//! A future `auth.user_external_identity` side-table will store
//! provenance per external user (source, issuer, external_sub,
//! last_verified_at). When that lands, this hook will:
//!   - `on_user_created` → INSERT the provenance row.
//!   - `on_user_login` → UPDATE `last_verified_at`.
//!   - `on_user_deleted` → no extra work (FK CASCADE handles it).
//!
//! The current implementation reserves the slot without committing to
//! the schema yet.

use std::sync::Arc;

use async_trait::async_trait;

use crate::application::ports::user_lifecycle::{DeletionMode, LogoutReason, UserLifecycleHook};
use crate::common::errors::DomainError;
use crate::domain::entities::user::User;
use crate::domain::repositories::magic_link_token_repository::MagicLinkTokenRepository;

pub struct ExternalIdentityLifecycleHook {
    /// `None` when the magic-link feature is disabled in this build —
    /// the cleanup path becomes a no-op. Production DI always wires this.
    magic_link_repo: Option<Arc<dyn MagicLinkTokenRepository>>,
}

impl ExternalIdentityLifecycleHook {
    /// Construct a no-op hook. Used by test stubs that don't exercise
    /// the magic-link path.
    pub fn new() -> Self {
        Self {
            magic_link_repo: None,
        }
    }

    /// Wire the magic-link token repo. Called by DI when the magic-link
    /// feature is enabled (PR 8 onwards).
    pub fn with_magic_link_repo(mut self, repo: Arc<dyn MagicLinkTokenRepository>) -> Self {
        self.magic_link_repo = Some(repo);
        self
    }
}

impl Default for ExternalIdentityLifecycleHook {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl UserLifecycleHook for ExternalIdentityLifecycleHook {
    fn name(&self) -> &'static str {
        "external_identity"
    }

    async fn on_user_created(&self, user: &User) -> Result<(), DomainError> {
        // Only externals are interesting to this hook — internal users go
        // through the regular registration path that the audit hook
        // already records. Future PRs (provenance side-table) will turn
        // this into a SQL INSERT.
        if user.is_external() {
            tracing::info!(
                target: "audit",
                event = "external_user.created",
                user_id = %user.id(),
                username = %user.display_for_audit(),
                email = %user.email(),
            );
        }
        Ok(())
    }

    async fn on_user_login(&self, user: &User) -> Result<(), DomainError> {
        if user.is_external() {
            tracing::info!(
                target: "audit",
                event = "external_user.login",
                user_id = %user.id(),
                username = %user.display_for_audit(),
                first_login = user.last_login_at().is_none(),
            );
        }
        Ok(())
    }

    async fn on_user_logout(&self, _user: &User, _reason: LogoutReason) -> Result<(), DomainError> {
        // Provenance is connection-level, not session-level. No work today.
        Ok(())
    }

    async fn on_user_deleted(
        &self,
        user: &User,
        _mode: DeletionMode,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), DomainError> {
        // Best-effort cleanup of outstanding magic-link tokens. The
        // `ON DELETE CASCADE` on the FK would handle this automatically
        // after the user row is removed — calling it explicitly inside
        // the same transaction lets us record an audit count, and
        // ensures the cleanup is visible to any subsequent hook in the
        // same dispatcher chain.
        if let Some(repo) = &self.magic_link_repo {
            let removed = repo.delete_all_for_user_tx(user.id(), tx).await?;
            if removed > 0 {
                tracing::info!(
                    target: "audit",
                    event = "external_user.tokens_cleared",
                    user_id = %user.id(),
                    tokens_removed = removed,
                );
            }
        }
        Ok(())
    }
}
