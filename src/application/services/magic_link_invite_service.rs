//! Invite-by-email orchestration for `POST /api/grants` with
//! `subject.type = "email"`.
//!
//! Two-step API (kept separate so the handler can interleave the standard
//! grant-creation step in between):
//!
//!   1. [`resolve_or_create_recipient`] — normalise the email, apply the
//!      allowlist + kill-switch checks, then look up or lazily provision
//!      an external user. Returns the resolved [`User`] entity.
//!   2. [`issue_invitation`] — mint a magic-link token targeting the
//!      shared resource, build the `/magic/v1/{token}` URL, and send the
//!      invitation email through the wired `EmailSender`.
//!
//! Step 2 is only called when the resolved user has no other login
//! credential (`!user.has_login_credential()`) — internal users with
//! passwords / OIDC see the grant appear in their normal
//! "Shared with me" view and do not get a clickable magic link.
//!
//! # Enumeration defense
//!
//! v1 awaits the SMTP send synchronously. A malicious caller can in
//! theory measure response times to distinguish "new external user
//! provisioned + mail sent" from "existing internal user, no mail" —
//! a single-bit oracle. The plan defers full constant-time defense
//! (fire-and-forget spawn, dummy SMTP latency on no-op paths) to PR 12.

use std::sync::Arc;

use chrono::Utc;

use crate::application::ports::email_sender::{EmailMessage, EmailSender};
use crate::application::services::user_lifecycle_service::UserLifecycleService;
use crate::common::config::MagicLinkConfig;
use crate::common::errors::{DomainError, ErrorKind};
use crate::domain::entities::magic_link_token::{MagicLinkResourceKind, MagicLinkToken};
use crate::domain::entities::user::{User, UserRole};
use crate::domain::repositories::magic_link_token_repository::MagicLinkTokenRepository;
use crate::domain::repositories::user_repository::{UserRepository, UserRepositoryError};
use crate::domain::services::authorization::{Resource, ResourceKind};
use crate::domain::services::email_normalize::normalize_email;
use crate::infrastructure::repositories::pg::UserPgRepository;

pub struct MagicLinkInviteService {
    user_storage: Arc<UserPgRepository>,
    magic_link_repo: Arc<dyn MagicLinkTokenRepository>,
    email_sender: Arc<dyn EmailSender>,
    user_lifecycle: Arc<UserLifecycleService>,
    magic_link_cfg: MagicLinkConfig,
    /// Public base URL of this OxiCloud instance — used to build the
    /// `/magic/v1/{token}` invitation link. Sourced from
    /// `AppConfig::base_url()` at DI time.
    public_base_url: String,
}

impl MagicLinkInviteService {
    pub fn new(
        user_storage: Arc<UserPgRepository>,
        magic_link_repo: Arc<dyn MagicLinkTokenRepository>,
        email_sender: Arc<dyn EmailSender>,
        user_lifecycle: Arc<UserLifecycleService>,
        magic_link_cfg: MagicLinkConfig,
        public_base_url: String,
    ) -> Self {
        Self {
            user_storage,
            magic_link_repo,
            email_sender,
            user_lifecycle,
            magic_link_cfg,
            public_base_url,
        }
    }

    /// Resolve the email to an existing user, or lazily provision a new
    /// external user. Returns the resolved [`User`].
    ///
    /// Errors:
    /// - `InvalidInput` — email failed normalisation (malformed / too long).
    /// - `AccessDenied` — email-grant kill switch is off
    ///   (`OXICLOUD_ALLOW_EXTERNAL_USERS=false`) and no matching user
    ///   exists, OR the email's domain isn't in the allowlist.
    /// - any propagated repo error.
    pub async fn resolve_or_create_recipient(&self, raw_email: &str) -> Result<User, DomainError> {
        let normalised = normalize_email(raw_email).map_err(|e| {
            DomainError::new(ErrorKind::InvalidInput, "MagicLinkInvite", format!("{}", e))
        })?;

        // Fast path: existing user with this email — works for both
        // internal (was previously created via normal registration) and
        // external (previous invitation re-sharing) cases.
        match UserRepository::get_user_by_email(&*self.user_storage, &normalised).await {
            Ok(user) => Ok(user),
            Err(UserRepositoryError::NotFound(_)) => self.create_external_user(&normalised).await,
            Err(e) => Err(DomainError::from(e)),
        }
    }

    /// Lazy provisioning path. Runs the two policy guards (kill switch
    /// and per-domain allowlist) before touching the DB.
    async fn create_external_user(&self, normalised_email: &str) -> Result<User, DomainError> {
        if !self.magic_link_cfg.allow_external_users {
            return Err(DomainError::new(
                ErrorKind::AccessDenied,
                "MagicLinkInvite",
                "Creating external users is disabled on this server \
                 (OXICLOUD_ALLOW_EXTERNAL_USERS=false)"
                    .to_string(),
            ));
        }
        if !self.magic_link_cfg.is_email_allowed(normalised_email) {
            return Err(DomainError::new(
                ErrorKind::AccessDenied,
                "MagicLinkInvite",
                format!(
                    "Email domain is not in the allowlist (OXICLOUD_EXTERNAL_EMAIL_DOMAINS); \
                     refusing to invite {}",
                    normalised_email,
                ),
            ));
        }

        // External users are created without a username or password.
        // `password_hash IS NULL` is the canonical no-password marker.
        let user = User::new(
            normalised_email.to_string(),
            None,
            None,
            None,
            None,
            UserRole::User,
            0,
            true,
        )
        .map_err(|e| {
            DomainError::new(
                ErrorKind::InvalidInput,
                "MagicLinkInvite",
                format!("invalid external user data: {}", e),
            )
        })?;

        let saved = UserRepository::create_user(&*self.user_storage, user.clone())
            .await
            .map_err(DomainError::from)?;

        // Fire the user-lifecycle dispatcher — `on_user_created` lights
        // up audit + future external-identity provenance bookkeeping.
        // Errors are logged-and-continued by the dispatcher's
        // `dispatch_created` per the lifecycle contract.
        self.user_lifecycle.dispatch_created(&saved).await;

        Ok(saved)
    }

    /// Mint a magic-link token targeting the resource and email the
    /// invitation link. Caller is expected to have already created the
    /// grant rows.
    ///
    /// `inviter_username` is interpolated into the subject line as a
    /// trust signal ("Alice shared with you on OxiCloud"). The message
    /// body is plain text only in v1; HTML templating is out of scope
    /// (see plan "Out of scope" → "Email template engine").
    pub async fn issue_invitation(
        &self,
        recipient: &User,
        inviter_username: &str,
        resource: Resource,
    ) -> Result<(), DomainError> {
        // The grant is in place either way; only mint a magic link when
        // the recipient has no other way to authenticate (the auto-auth
        // mailbox-as-2FA-bypass is only acceptable when the recipient
        // has nothing else). Internal users with passwords / OIDC
        // simply see the grant in their normal "Shared with me" view.
        if recipient.has_login_credential() {
            return Ok(());
        }

        let (kind, resource_id) = match resource {
            Resource::Folder(id) => (MagicLinkResourceKind::Folder, id),
            Resource::File(id) => (MagicLinkResourceKind::File, id),
        };
        let token = MagicLinkToken::new(
            recipient.id(),
            self.magic_link_cfg.ttl_hours,
            Some((kind, resource_id)),
        );
        self.magic_link_repo.create(&token).await?;

        let link = format!(
            "{}/magic/v1/{}",
            self.public_base_url.trim_end_matches('/'),
            token.token(),
        );

        let kind_label = match resource {
            Resource::Folder(_) => "folder",
            Resource::File(_) => "file",
        };
        let subject = format!(
            "{} shared a {} with you on OxiCloud",
            inviter_username, kind_label
        );
        let text_body = format!(
            "{inviter} shared a {kind} with you on OxiCloud.\n\
             \n\
             Open it by clicking the link below:\n\
             {link}\n\
             \n\
             The link works once and expires in {ttl} hours.\n\
             If you didn't expect this invitation, you can safely ignore this message.\n\
             \n\
             — OxiCloud, {now}\n",
            inviter = inviter_username,
            kind = kind_label,
            link = link,
            ttl = self.magic_link_cfg.ttl_hours,
            now = Utc::now().to_rfc3339(),
        );

        let message = EmailMessage {
            to: recipient.email().to_string(),
            subject,
            text_body,
            html_body: None,
        };

        // Synchronous send — see module docs for the enumeration-defense
        // trade-off. PR 12 promotes this to fire-and-forget when the
        // hardening pass lands.
        match self.email_sender.send(message).await {
            Ok(outcome) => {
                tracing::info!(
                    target: "audit",
                    event = "magic_link.invitation_sent",
                    recipient_user_id = %recipient.id(),
                    recipient_email = %recipient.email(),
                    resource = ?resource,
                    smtp_code = outcome.code,
                    smtp_message = %outcome.message,
                );
                Ok(())
            }
            Err(e) => {
                // The grant already exists — log the SMTP failure but
                // don't propagate it as a fatal error, so the API client
                // still gets `201 Created` with the GrantDto. Recipient
                // can re-trigger via the future `POST /api/auth/magic-link/send`
                // endpoint once login-via-email lands.
                tracing::warn!(
                    target: "audit",
                    event = "magic_link.invitation_send_failed",
                    recipient_user_id = %recipient.id(),
                    recipient_email = %recipient.email(),
                    error = %e.message,
                );
                Ok(())
            }
        }
    }

    /// Login-via-email flow (PR 10). Caller submits an email at
    /// `/login`; we look it up — **never lazy-create** here, that path
    /// is reserved for `resolve_or_create_recipient` — and if the
    /// matched user has no other login credential, mint a NULL-resource
    /// magic-link token and email a sign-in link. The redemption
    /// endpoint lands a NULL-resource token on `/#/sharedwithme`.
    ///
    /// Always returns `Ok(())` so the caller can emit a uniform
    /// response shape (`"If an account exists, a link will be sent."`)
    /// that doesn't reveal whether the email maps to an account.
    ///
    /// Audit log distinguishes three real outcomes — `sent`,
    /// `no_account`, `has_credential` — so operators can see the truth
    /// while the API stays anti-enumeration-safe. A fourth outcome
    /// `send_failed` is logged at `warn` level when SMTP errors.
    pub async fn send_login_link(&self, raw_email: &str) -> Result<(), DomainError> {
        let normalised = match normalize_email(raw_email) {
            Ok(n) => n,
            Err(e) => {
                // Malformed input is treated the same as "no account"
                // — uniform response, no oracle from validation errors.
                tracing::info!(
                    target: "audit",
                    event = "auth.magic_link_send",
                    reason = "malformed_email",
                    error = %e,
                    "🔗 login-link suppressed: malformed email",
                );
                return Ok(());
            }
        };

        let user = match UserRepository::get_user_by_email(&*self.user_storage, &normalised).await {
            Ok(u) => u,
            Err(UserRepositoryError::NotFound(_)) => {
                tracing::info!(
                    target: "audit",
                    event = "auth.magic_link_send",
                    reason = "no_account",
                    email = %normalised,
                    "🔗 login-link suppressed: no account for '{}'",
                    normalised,
                );
                return Ok(());
            }
            Err(e) => return Err(DomainError::from(e)),
        };

        if user.has_login_credential() {
            // Refuse the magic-link path for users with a password /
            // OIDC — accepting it would let an attacker bypass those
            // factors by merely owning the mailbox at the moment of
            // request. They should sign in through the regular form.
            tracing::info!(
                target: "audit",
                event = "auth.magic_link_send",
                reason = "has_credential",
                user_id = %user.id(),
                username = %user.display_for_audit(),
                email = %normalised,
                "🔗 login-link suppressed: '{}' has another login credential",
                user.display_for_audit(),
            );
            return Ok(());
        }

        if !user.is_active() {
            tracing::info!(
                target: "audit",
                event = "auth.magic_link_send",
                reason = "account_deactivated",
                user_id = %user.id(),
                username = %user.display_for_audit(),
                email = %normalised,
                "🔗 login-link suppressed: account deactivated for '{}'",
                user.display_for_audit(),
            );
            return Ok(());
        }

        // Mint a NULL-resource token. The redemption handler lands
        // NULL-resource tokens on /#/sharedwithme (see PR 8).
        let token = MagicLinkToken::new(user.id(), self.magic_link_cfg.ttl_hours, None);
        self.magic_link_repo.create(&token).await?;

        let link = format!(
            "{}/magic/v1/{}",
            self.public_base_url.trim_end_matches('/'),
            token.token(),
        );
        let subject = "Sign in to OxiCloud".to_string();
        let text_body = format!(
            "Hello,\n\
             \n\
             Use the link below to sign in to OxiCloud. The link works \
             once and expires in {ttl} hours.\n\
             \n\
             {link}\n\
             \n\
             If you didn't request this sign-in link, you can safely \
             ignore this message — no further action is needed.\n\
             \n\
             — OxiCloud, {now}\n",
            ttl = self.magic_link_cfg.ttl_hours,
            link = link,
            now = Utc::now().to_rfc3339(),
        );

        let message = EmailMessage {
            to: user.email().to_string(),
            subject,
            text_body,
            html_body: None,
        };

        match self.email_sender.send(message).await {
            Ok(outcome) => {
                tracing::info!(
                    target: "audit",
                    event = "auth.magic_link_send",
                    reason = "sent",
                    user_id = %user.id(),
                    username = %user.display_for_audit(),
                    email = %normalised,
                    smtp_code = outcome.code,
                    smtp_message = %outcome.message,
                    "🔗 login-link sent to '{}'",
                    normalised,
                );
            }
            Err(e) => {
                tracing::warn!(
                    target: "audit",
                    event = "auth.magic_link_send_failed",
                    user_id = %user.id(),
                    email = %normalised,
                    error = %e.message,
                    "🔗 login-link SMTP send failed for '{}'",
                    normalised,
                );
            }
        }

        Ok(())
    }
}

/// Lightweight conversion so the grant handler can derive a
/// [`MagicLinkResourceKind`] from the already-parsed [`ResourceKind`]
/// without re-importing match arms.
impl From<ResourceKind> for MagicLinkResourceKind {
    fn from(kind: ResourceKind) -> Self {
        match kind {
            ResourceKind::Folder => Self::Folder,
            ResourceKind::File => Self::File,
        }
    }
}
