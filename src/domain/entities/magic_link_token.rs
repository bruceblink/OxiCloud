//! Magic-link authentication tokens.
//!
//! Two distinct flows mint these tokens:
//!
//! - **Invitation** (PR 9). An internal user shares a resource with an email
//!   address. If the recipient has no account yet, an external user is
//!   lazily provisioned and a token is minted pointing at the resource.
//!   Mail with `/magic/v1/{token}` is delivered; clicking the link
//!   authenticates the recipient and 302s them to the resource.
//!
//! - **Login-via-email** (PR 10). A user without any other credential (an
//!   already-existing external user who hasn't set a password) requests a
//!   login link from `/login`. Token has NO resource target; redemption
//!   lands on `/shared-with-me`.
//!
//! The two flows share the same redemption endpoint — the deep-link
//! decision is made by inspecting whether `resource_type/resource_id` are
//! present on the token row.
//!
//! Single-use is enforced by the `status` enum transitioning from
//! `Pending` → `Used` exactly once. The redemption endpoint runs the
//! transition inside a SQL transaction (`UPDATE ... WHERE status='pending'`
//! returning the row) so concurrent redemption attempts can't both
//! succeed.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use rand_core::{OsRng, RngCore};
use uuid::Uuid;

/// Resource targeted by an invitation token. Mirrors
/// `domain::services::authorization::ResourceKind` but is duplicated here
/// to keep the entity self-contained (no auth-domain dependency).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MagicLinkResourceKind {
    File,
    Folder,
}

impl MagicLinkResourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Folder => "folder",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "file" => Some(Self::File),
            "folder" => Some(Self::Folder),
            _ => None,
        }
    }
}

/// Lifecycle state of a magic-link token. Strict one-way transitions:
/// `Pending → Used` (successful redemption) or `Pending → Expired`
/// (background sweep after `expires_at`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MagicLinkStatus {
    Pending,
    Used,
    Expired,
}

impl MagicLinkStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Used => "used",
            Self::Expired => "expired",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "used" => Some(Self::Used),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

impl std::fmt::Display for MagicLinkStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Domain entity for a magic-link token row.
#[derive(Debug, Clone)]
pub struct MagicLinkToken {
    id: Uuid,
    /// 32 bytes of CSPRNG output, URL-safe base64 (no padding), ≈43 chars.
    token: String,
    user_id: Uuid,
    status: MagicLinkStatus,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    used_at: Option<DateTime<Utc>>,
    /// Optional deep-link target. Both `Some` together → invitation flow;
    /// both `None` together → login-via-email flow. Mismatched is a
    /// schema-level error guarded by the DB CHECK `magic_link_tokens_resource_pair`.
    resource_kind: Option<MagicLinkResourceKind>,
    resource_id: Option<Uuid>,
    /// Per-request challenge (PR 22). Mirrors the `oxicloud_magic_request`
    /// cookie set on the originating browser when the user requests a
    /// login-via-email link. Compared on redemption to bind the
    /// magic-link to the device that requested it.
    ///
    /// `Some` for login-via-email tokens (browser-bound); `None` for
    /// invitation tokens (cross-device by design — recipient has no
    /// prior browser context with the server).
    request_challenge: Option<String>,
}

impl MagicLinkToken {
    /// Mint a fresh pending token. Generates 32 CSPRNG bytes, encodes them
    /// URL-safe base64 (no padding), and stamps `issued_at = now`,
    /// `expires_at = now + ttl`.
    ///
    /// `resource` is `Some((kind, id))` for invitations (deep-link to a
    /// specific file/folder) or `None` for login-via-email (lands on
    /// `/shared-with-me` or `/files` depending on `is_external`).
    ///
    /// `request_challenge` carries the per-request value mirrored into
    /// the originating browser's cookie. Pass `Some` for login-via-email
    /// (browser-bound), `None` for invitations (cross-device).
    pub fn new(
        user_id: Uuid,
        ttl: Duration,
        resource: Option<(MagicLinkResourceKind, Uuid)>,
        request_challenge: Option<String>,
    ) -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let token = URL_SAFE_NO_PAD.encode(bytes);

        let now = Utc::now();
        let (resource_kind, resource_id) = match resource {
            Some((k, id)) => (Some(k), Some(id)),
            None => (None, None),
        };

        Self {
            id: Uuid::new_v4(),
            token,
            user_id,
            status: MagicLinkStatus::Pending,
            issued_at: now,
            expires_at: now + ttl,
            used_at: None,
            resource_kind,
            resource_id,
            request_challenge,
        }
    }

    /// Reconstruct from a database row.
    #[allow(clippy::too_many_arguments)]
    pub fn from_raw(
        id: Uuid,
        token: String,
        user_id: Uuid,
        status: MagicLinkStatus,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
        used_at: Option<DateTime<Utc>>,
        resource_kind: Option<MagicLinkResourceKind>,
        resource_id: Option<Uuid>,
        request_challenge: Option<String>,
    ) -> Self {
        Self {
            id,
            token,
            user_id,
            status,
            issued_at,
            expires_at,
            used_at,
            resource_kind,
            resource_id,
            request_challenge,
        }
    }

    // ── Getters ──────────────────────────────────────────────────

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn user_id(&self) -> Uuid {
        self.user_id
    }

    pub fn status(&self) -> MagicLinkStatus {
        self.status
    }

    pub fn issued_at(&self) -> DateTime<Utc> {
        self.issued_at
    }

    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }

    pub fn used_at(&self) -> Option<DateTime<Utc>> {
        self.used_at
    }

    pub fn resource_kind(&self) -> Option<MagicLinkResourceKind> {
        self.resource_kind
    }

    pub fn resource_id(&self) -> Option<Uuid> {
        self.resource_id
    }

    /// Per-request challenge for browser binding (PR 22). `Some` for
    /// login-via-email tokens — the redemption endpoint compares this
    /// with the inbound `oxicloud_magic_request` cookie. `None` for
    /// invitation tokens — they bypass the cookie check entirely.
    pub fn request_challenge(&self) -> Option<&str> {
        self.request_challenge.as_deref()
    }

    // ── Business logic ───────────────────────────────────────────

    /// `true` once `expires_at < now`. The status column may still be
    /// `Pending` if the background sweep hasn't run yet; treat this
    /// method as authoritative at redemption time.
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }

    /// `true` iff the token is in a state where it can be redeemed
    /// (pending + not yet past TTL). The redemption endpoint should
    /// check this; the DB-level `UPDATE WHERE status='pending'` is the
    /// definitive single-use guard.
    pub fn is_redeemable(&self) -> bool {
        self.status == MagicLinkStatus::Pending && !self.is_expired()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_token_is_pending_and_within_ttl() {
        let user_id = Uuid::new_v4();
        let token = MagicLinkToken::new(user_id, Duration::hours(24), None, None);
        assert_eq!(token.status(), MagicLinkStatus::Pending);
        assert_eq!(token.user_id(), user_id);
        assert!(token.resource_kind().is_none());
        assert!(token.resource_id().is_none());
        assert!(token.is_redeemable());
        assert!(!token.is_expired());
        // 32 bytes → 43 chars URL-safe base64 (no padding).
        assert_eq!(token.token().len(), 43);
    }

    #[test]
    fn new_token_with_resource_carries_both_fields() {
        let user_id = Uuid::new_v4();
        let folder_id = Uuid::new_v4();
        let token = MagicLinkToken::new(
            user_id,
            Duration::hours(24),
            Some((MagicLinkResourceKind::Folder, folder_id)),
            None,
        );
        assert_eq!(token.resource_kind(), Some(MagicLinkResourceKind::Folder));
        assert_eq!(token.resource_id(), Some(folder_id));
    }

    #[test]
    fn each_token_is_unique() {
        let user_id = Uuid::new_v4();
        let a = MagicLinkToken::new(user_id, Duration::hours(24), None, None);
        let b = MagicLinkToken::new(user_id, Duration::hours(24), None, None);
        assert_ne!(a.token(), b.token());
        assert_ne!(a.id(), b.id());
    }

    #[test]
    fn status_round_trip() {
        for s in [
            MagicLinkStatus::Pending,
            MagicLinkStatus::Used,
            MagicLinkStatus::Expired,
        ] {
            assert_eq!(MagicLinkStatus::parse(s.as_str()), Some(s));
        }
    }
}
