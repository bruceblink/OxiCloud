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
//! Step 2 is gated by [`magic_link_eligibility`] — OIDC users are
//! unconditionally rejected (audit `oidc_user`); password users are
//! rejected by default (`has_password`) but allowed when
//! `OXICLOUD_MAGIC_LINK_OPEN_TO_PASSWORD_USERS=true`. Rejected
//! invitations still result in the grant being created — the recipient
//! sees the shared resource in their normal "Shared with me" view —
//! only the courtesy notification mail is suppressed.
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

/// Eligibility decision for a user to receive a magic-link.
///
/// Returned by [`magic_link_eligibility`]. The `Reject` arm carries a
/// **stable** audit-reason key (`"oidc_user"`, `"has_password"`,
/// `"account_deactivated"`) — log aggregators key off this, do not
/// repurpose existing values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eligibility {
    Allow,
    Reject(&'static str),
}

/// Decide whether to mint a magic-link for the given user.
///
/// Precedence ladder (PR 19):
///
/// 1. **OIDC linked** → always reject with `"oidc_user"`. The IdP is the
///    security boundary and may enforce MFA that magic-link would
///    bypass. The `open_to_password_users` flag has **no effect**.
/// 2. **Has a password configured** → reject with `"has_password"` by
///    default. Allow when `open_to_password_users` is `true` (lenient
///    mode — operator opt-in via env, accepting that mailbox compromise
///    becomes equivalent to password compromise).
/// 3. **No credential at all** (the typical external user or
///    fresh email-only signup) → allow.
///
/// Account-deactivation is **not** checked here — `send_login_link` /
/// `issue_invitation` handle it separately because the rejection reason
/// (`"account_deactivated"`) is unrelated to credential state.
pub fn magic_link_eligibility(user: &User, open_to_password_users: bool) -> Eligibility {
    if user.is_oidc_user() {
        return Eligibility::Reject("oidc_user");
    }
    if user.has_password() {
        return if open_to_password_users {
            Eligibility::Allow
        } else {
            Eligibility::Reject("has_password")
        };
    }
    Eligibility::Allow
}

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
        // the recipient is magic-link-eligible. OIDC-linked users never
        // get one (IdP is the security boundary); password users get
        // one only when the operator opted into lenient mode via
        // `OXICLOUD_MAGIC_LINK_OPEN_TO_PASSWORD_USERS=true`. Either way
        // they see the grant in their normal "Shared with me" view —
        // the mail is purely a notification convenience.
        if let Eligibility::Reject(reason) =
            magic_link_eligibility(recipient, self.magic_link_cfg.open_to_password_users)
        {
            tracing::info!(
                target: "audit",
                event = "magic_link.invitation_suppressed",
                reason = reason,
                user_id = %recipient.id(),
                username = %recipient.display_for_audit(),
                "📭 invitation mail suppressed: '{}' is not magic-link-eligible ({})",
                recipient.display_for_audit(),
                reason,
            );
            return Ok(());
        }

        let (kind, resource_id) = match resource {
            Resource::Folder(id) => (MagicLinkResourceKind::Folder, id),
            Resource::File(id) => (MagicLinkResourceKind::File, id),
        };
        // Invitation tokens are cross-device by design (recipient has
        // no prior browser context with the server) — no challenge
        // cookie. Long TTL (default 24h) because recipients may not
        // check their email for a while.
        let token = MagicLinkToken::new(
            recipient.id(),
            chrono::Duration::hours(self.magic_link_cfg.invite_ttl_hours as i64),
            Some((kind, resource_id)),
            None,
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
            ttl = self.magic_link_cfg.invite_ttl_hours,
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
    /// `no_account`, `oidc_user`, `has_password` — so operators can see the truth
    /// while the API stays anti-enumeration-safe. A fourth outcome
    /// `send_failed` is logged at `warn` level when SMTP errors.
    ///
    /// `request_challenge` is the per-request random value the handler
    /// already set as the `oxicloud_magic_request` cookie on the
    /// originating browser. The service mirrors it into the token row;
    /// the redemption endpoint compares it against the inbound cookie
    /// to bind the magic-link to the device that requested it.
    /// Anti-enumeration: the handler passes the same challenge whether
    /// or not the user exists / is eligible — the token row is just
    /// not created in those branches, so nothing is leaked by the
    /// presence or absence of the cookie.
    pub async fn send_login_link(
        &self,
        raw_email: &str,
        request_challenge: &str,
    ) -> Result<(), DomainError> {
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

        if let Eligibility::Reject(reason) =
            magic_link_eligibility(&user, self.magic_link_cfg.open_to_password_users)
        {
            // Refuse the magic-link path for users who have a stronger
            // credential configured. OIDC is unconditional — the IdP is
            // the security boundary and we must not bypass any MFA it
            // enforces. Password is gated by `open_to_password_users`:
            // strict mode refuses (default — magic-link would weaken the
            // password to mailbox-strength); lenient mode allows.
            tracing::info!(
                target: "audit",
                event = "auth.magic_link_send",
                reason = reason,
                user_id = %user.id(),
                username = %user.display_for_audit(),
                email = %normalised,
                "🔗 login-link suppressed: '{}' rejected ({})",
                user.display_for_audit(),
                reason,
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

        // Mint a NULL-resource token bound to the requesting browser
        // via `request_challenge` (PR 22). Short TTL (default 10 min)
        // — the user just clicked the button, so a slow click is
        // almost certainly someone else with access to the inbox.
        let token = MagicLinkToken::new(
            user.id(),
            chrono::Duration::minutes(self.magic_link_cfg.login_ttl_minutes as i64),
            None,
            Some(request_challenge.to_string()),
        );
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
             once and expires in {ttl} minutes. Open it on the same \
             device where you requested it.\n\
             \n\
             {link}\n\
             \n\
             If you didn't request this sign-in link, you can safely \
             ignore this message — no further action is needed.\n\
             \n\
             — OxiCloud, {now}\n",
            ttl = self.magic_link_cfg.login_ttl_minutes,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::entities::user::{User, UserRole};

    fn user(password: Option<&str>, oidc: Option<(&str, &str)>) -> User {
        let (provider, subject) = match oidc {
            Some((p, s)) => (Some(p.to_string()), Some(s.to_string())),
            None => (None, None),
        };
        User::new(
            "test@example.com".to_string(),
            None,
            password.map(str::to_string),
            provider,
            subject,
            UserRole::User,
            0,
            true,
        )
        .expect("test user")
    }

    #[test]
    fn oidc_always_rejected_regardless_of_flag() {
        let u = user(None, Some(("google", "sub-123")));
        assert_eq!(
            magic_link_eligibility(&u, false),
            Eligibility::Reject("oidc_user")
        );
        assert_eq!(
            magic_link_eligibility(&u, true),
            Eligibility::Reject("oidc_user")
        );
    }

    #[test]
    fn password_user_strict_then_lenient() {
        let u = user(Some("$argon2id$..."), None);
        assert_eq!(
            magic_link_eligibility(&u, false),
            Eligibility::Reject("has_password")
        );
        assert_eq!(magic_link_eligibility(&u, true), Eligibility::Allow);
    }

    #[test]
    fn no_credential_always_allowed() {
        let u = user(None, None);
        assert_eq!(magic_link_eligibility(&u, false), Eligibility::Allow);
        assert_eq!(magic_link_eligibility(&u, true), Eligibility::Allow);
    }

    #[test]
    fn oidc_dominates_password_when_both_set() {
        // Edge case: user has password AND OIDC linked. The ladder
        // checks OIDC first, so the rejection reason is "oidc_user"
        // (not "has_password"). The flag doesn't matter here either.
        let u = user(Some("hash"), Some(("google", "sub-123")));
        assert_eq!(
            magic_link_eligibility(&u, false),
            Eligibility::Reject("oidc_user")
        );
        assert_eq!(
            magic_link_eligibility(&u, true),
            Eligibility::Reject("oidc_user")
        );
    }
}
