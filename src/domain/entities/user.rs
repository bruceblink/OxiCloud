use chrono::{DateTime, Utc};
use uuid::Uuid;

// Re-export entity errors from the centralized module
pub use super::entity_errors::{UserError, UserResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// We'll handle conversion manually for now until the type is properly set up in the database
pub enum UserRole {
    Admin,
    User,
}

impl std::fmt::Display for UserRole {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            UserRole::Admin => write!(f, "admin"),
            UserRole::User => write!(f, "user"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct User {
    id: Uuid,
    /// Optional handle (2-64 chars, no `@`). NULL for users created via
    /// email-invitation (`is_external = true`) and for users who have
    /// not yet claimed a handle (PR-18 email-only signups). When set, it
    /// must satisfy `validate_username` and must NOT contain `@` —
    /// keeping the username and email namespaces provably disjoint.
    username: Option<String>,
    email: String,
    /// Optional Argon2 password hash. NULL when the user has no password
    /// (externals, OIDC-only users, email-only signups awaiting their
    /// welcome magic-link). After PR 16 this column carries no sentinel
    /// strings — `is_some()` means "real argon2 hash"; `None` means "no
    /// password configured".
    password_hash: Option<String>,
    role: UserRole,
    storage_quota_bytes: i64,
    storage_used_bytes: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    last_login_at: Option<DateTime<Utc>>,
    active: bool,
    oidc_provider: Option<String>,
    oidc_subject: Option<String>,
    image: Option<String>,
    /// TRUE = grant-only external recipient (magic-link, OIDC-only, OCM
    /// federated). FALSE = storage-owning internal user. Hooks that
    /// provision per-user resources (home folder, default calendar, …)
    /// must short-circuit when `is_external` is TRUE — see tip #2 in
    /// `application/ports/user_lifecycle.rs`. The DB CHECK constraint
    /// `users_external_no_storage` is the schema-level safety net.
    is_external: bool,
    /// Optional human-readable first/given name. Populated from OIDC
    /// standard claim `given_name` at JIT provisioning, or via the
    /// profile-edit endpoint. External users start with `None`.
    given_name: Option<String>,
    /// Optional human-readable last/family name. Populated from OIDC
    /// standard claim `family_name` at JIT provisioning, or via the
    /// profile-edit endpoint. External users start with `None`.
    family_name: Option<String>,
}

impl User {
    /// Create a new user.
    ///
    /// One unified constructor for every kind of user (internal, OIDC-linked,
    /// external). The credential slots and the `is_external` marker are all
    /// caller-controlled — what makes a user "OIDC" is `oidc_subject =
    /// Some(_)`, what makes them "external" is `is_external = true`. There
    /// are no hidden sentinel values; an absent credential is `None`.
    ///
    /// # Arguments
    /// * `email` — required, must satisfy `validate_email`
    /// * `username` — optional handle (2-64 chars, no `@`)
    /// * `password_hash` — pre-hashed via PasswordHasherPort, or `None` if
    ///   the user has no password yet (magic-link or OIDC bootstrap)
    /// * `oidc_provider`, `oidc_subject` — both `Some` when the user is
    ///   linked to an external IdP, both `None` otherwise
    /// * `role` — `Admin` is rejected when `is_external = true` (mirrors the
    ///   `users_external_not_admin` DB CHECK constraint)
    /// * `storage_quota_bytes` — caller-set; external callers should pass 0
    ///   to satisfy the `users_external_no_storage` invariant
    /// * `is_external` — TRUE for grant-only recipients (magic-link, OCM)
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        email: String,
        username: Option<String>,
        password_hash: Option<String>,
        oidc_provider: Option<String>,
        oidc_subject: Option<String>,
        role: UserRole,
        storage_quota_bytes: i64,
        is_external: bool,
    ) -> UserResult<Self> {
        Self::validate_email(&email)?;
        if let Some(ref u) = username {
            Self::validate_username(u)?;
        }
        if let Some(ref h) = password_hash
            && h.is_empty()
        {
            return Err(UserError::InvalidPassword(
                "Password hash cannot be empty".to_string(),
            ));
        }
        // Schema-level CHECKs are mirrored at the entity layer so callers
        // get a typed error instead of an opaque DB rejection.
        if is_external && matches!(role, UserRole::Admin) {
            return Err(UserError::ValidationError(
                "External users cannot hold the admin role".to_string(),
            ));
        }
        if is_external && storage_quota_bytes != 0 {
            return Err(UserError::ValidationError(
                "External users must have storage_quota_bytes = 0".to_string(),
            ));
        }
        // OIDC linkage is all-or-nothing: both provider and subject set,
        // or neither. The DB has a UNIQUE index on (provider, subject)
        // WHERE both non-NULL; partial state would corrupt that.
        if oidc_provider.is_some() != oidc_subject.is_some() {
            return Err(UserError::ValidationError(
                "oidc_provider and oidc_subject must both be set or both be None".to_string(),
            ));
        }

        let now = Utc::now();
        Ok(Self {
            id: Uuid::new_v4(),
            username,
            email,
            password_hash,
            role,
            storage_quota_bytes,
            storage_used_bytes: 0,
            created_at: now,
            updated_at: now,
            last_login_at: None,
            active: true,
            oidc_provider,
            oidc_subject,
            image: None,
            is_external,
            given_name: None,
            family_name: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_data(
        id: Uuid,
        username: Option<String>,
        email: String,
        password_hash: Option<String>,
        role: UserRole,
        storage_quota_bytes: i64,
        storage_used_bytes: i64,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
        last_login_at: Option<DateTime<Utc>>,
        active: bool,
    ) -> Self {
        Self {
            id,
            username,
            email,
            password_hash,
            role,
            storage_quota_bytes,
            storage_used_bytes,
            created_at,
            updated_at,
            last_login_at,
            active,
            oidc_provider: None,
            oidc_subject: None,
            image: None,
            // `from_data` is the minimal-args reconstruction path used by
            // tests and JWT-claim-based principal hydration (which doesn't
            // carry `is_external`). Default to FALSE — JWT-validated
            // principals are existing internal users; magic-link external
            // sessions take a different path that hydrates from DB via
            // `from_data_full`.
            is_external: false,
            given_name: None,
            family_name: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_data_full(
        id: Uuid,
        username: Option<String>,
        email: String,
        password_hash: Option<String>,
        role: UserRole,
        storage_quota_bytes: i64,
        storage_used_bytes: i64,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
        last_login_at: Option<DateTime<Utc>>,
        active: bool,
        oidc_provider: Option<String>,
        oidc_subject: Option<String>,
        image: Option<String>,
        is_external: bool,
        given_name: Option<String>,
        family_name: Option<String>,
    ) -> Self {
        Self {
            id,
            username,
            email,
            password_hash,
            role,
            storage_quota_bytes,
            storage_used_bytes,
            created_at,
            updated_at,
            last_login_at,
            active,
            oidc_provider,
            oidc_subject,
            image,
            is_external,
            given_name,
            family_name,
        }
    }

    // Getters
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// The user's chosen handle. `None` for users who have not claimed
    /// one (externals, fresh email-only signups). Display callers should
    /// fall back through `given_name`/`family_name` to `email` when this
    /// is `None`.
    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }

    pub fn email(&self) -> &str {
        &self.email
    }

    pub fn role(&self) -> UserRole {
        self.role
    }

    pub fn storage_quota_bytes(&self) -> i64 {
        self.storage_quota_bytes
    }

    pub fn storage_used_bytes(&self) -> i64 {
        self.storage_used_bytes
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }

    pub fn last_login_at(&self) -> Option<DateTime<Utc>> {
        self.last_login_at
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The Argon2 password hash, or `None` when the user has no password
    /// configured (externals, OIDC-only users, post-PR-18 email-only
    /// signups). `verify_password` callers must short-circuit to
    /// "invalid credentials" when this is `None`.
    pub fn password_hash(&self) -> Option<&str> {
        self.password_hash.as_deref()
    }

    /// Convenience: does the user have a real password configured?
    pub fn has_password(&self) -> bool {
        self.password_hash.is_some()
    }

    /// Best-effort label for audit-log interpolation. Returns the
    /// username when set; falls back to the user_id otherwise. Always
    /// implements `Display` (returns `String`) so audit lines can stay
    /// `username = %user.display_for_audit()` regardless of whether the
    /// user has claimed a handle. Reserve this for `target: "audit"`
    /// lines — user-facing display callers should walk the
    /// `username → given/family → email` fallback chain themselves.
    pub fn display_for_audit(&self) -> String {
        match &self.username {
            Some(u) => u.clone(),
            None => self.id.to_string(),
        }
    }

    pub fn oidc_provider(&self) -> Option<&str> {
        self.oidc_provider.as_deref()
    }

    pub fn oidc_subject(&self) -> Option<&str> {
        self.oidc_subject.as_deref()
    }

    pub fn image(&self) -> Option<&str> {
        self.image.as_deref()
    }

    /// `TRUE` for grant-only external recipients (magic-link, OIDC-only,
    /// OCM federated). Hooks provisioning per-user resources must
    /// short-circuit when this returns `true` — see tip #2 in
    /// `application/ports/user_lifecycle.rs`.
    pub fn is_external(&self) -> bool {
        self.is_external
    }

    pub fn given_name(&self) -> Option<&str> {
        self.given_name.as_deref()
    }

    pub fn family_name(&self) -> Option<&str> {
        self.family_name.as_deref()
    }

    pub fn set_image(&mut self, image: Option<String>) {
        self.image = image;
        self.updated_at = Utc::now();
    }

    pub fn set_given_name(&mut self, given_name: Option<String>) {
        self.given_name = given_name;
        self.updated_at = Utc::now();
    }

    pub fn set_family_name(&mut self, family_name: Option<String>) {
        self.family_name = family_name;
        self.updated_at = Utc::now();
    }

    /// Claim or change the username. Runs the same validation as the
    /// constructor — callers must still ensure uniqueness at the repo
    /// level. Bumps `updated_at`. Used by the post-create profile-edit
    /// endpoint so a user who started with `None` can claim a handle
    /// later, or change to a different one. The home folder name is NOT
    /// renamed: it was display text at creation; the folder is owned
    /// by `user_id`.
    pub fn set_username(&mut self, new_username: String) -> UserResult<()> {
        Self::validate_username(&new_username)?;
        self.username = Some(new_username);
        self.updated_at = Utc::now();
        Ok(())
    }

    /// Unset the username (return to `None`). Use sparingly — most
    /// users keep their handle once claimed. Mainly here so admin
    /// tooling can clear a problematic handle without deleting the
    /// account.
    pub fn clear_username(&mut self) {
        self.username = None;
        self.updated_at = Utc::now();
    }

    /// Returns true if this is an OIDC-only user (no password)
    pub fn is_oidc_user(&self) -> bool {
        self.oidc_provider.is_some()
    }

    /// Returns true iff this user has any non-magic-link authentication
    /// method available — either a real password hash, or a linked OIDC
    /// subject. Magic-link eligibility for "no other credential" mode is
    /// the negation of this; the `OXICLOUD_MAGIC_LINK_OPEN_TO_PASSWORD_USERS`
    /// flag widens the policy at the service layer (`magic_link_eligibility`).
    pub fn has_login_credential(&self) -> bool {
        self.password_hash.is_some() || self.oidc_subject.is_some()
    }

    /// Set the password hash. The new password must be hashed externally
    /// via `PasswordHasherPort` before calling this. Passing `None`
    /// clears the password (e.g. when a user opts back into magic-link-only
    /// auth).
    pub fn update_password_hash(&mut self, new_hash: Option<String>) {
        self.password_hash = new_hash;
        self.updated_at = Utc::now();
    }

    // Update storage usage
    pub fn update_storage_used(&mut self, storage_used_bytes: i64) {
        self.storage_used_bytes = storage_used_bytes;
        self.updated_at = Utc::now();
    }

    // Register login
    pub fn register_login(&mut self) {
        let now = Utc::now();
        self.last_login_at = Some(now);
        self.updated_at = now;
    }

    // Deactivate user
    pub fn deactivate(&mut self) {
        self.active = false;
        self.updated_at = Utc::now();
    }

    // Activate user
    pub fn activate(&mut self) {
        self.active = true;
        self.updated_at = Utc::now();
    }

    // ── Shared validation helpers ──────────────────────────────────────

    /// Usernames are 2-64 chars of `[A-Za-z0-9._-]`. The `@` character is
    /// explicitly forbidden — keeping the username and email namespaces
    /// provably disjoint is what closes the cross-collision attack class
    /// described in the auth-simplification plan (a user can never claim
    /// a handle that shadows another user's email). No leading/trailing
    /// dot or hyphen. The character set also prevents XSS payloads from
    /// being stored as usernames.
    fn validate_username(username: &str) -> UserResult<()> {
        let len = username.chars().count();
        if !(2..=64).contains(&len) {
            return Err(UserError::InvalidUsername(
                "Username must be between 2 and 64 characters".to_string(),
            ));
        }
        if username.contains('@') {
            return Err(UserError::InvalidUsername(
                "Username must not contain '@' — use the email field for email addresses"
                    .to_string(),
            ));
        }
        if !username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(UserError::InvalidUsername(
                "Username may only contain letters, digits, hyphens, underscores, and dots"
                    .to_string(),
            ));
        }
        if username.starts_with('.')
            || username.starts_with('-')
            || username.ends_with('.')
            || username.ends_with('-')
        {
            return Err(UserError::InvalidUsername(
                "Username must not start or end with a dot or hyphen".to_string(),
            ));
        }
        Ok(())
    }

    /// Basic but meaningful email validation:
    /// - Must contain exactly one `@`
    /// - Local part and domain must be non-empty
    /// - Domain must contain at least one dot
    /// - No angle brackets, spaces, or other characters used in XSS payloads
    fn validate_email(email: &str) -> UserResult<()> {
        let parts: Vec<&str> = email.splitn(2, '@').collect();
        if parts.len() != 2 {
            return Err(UserError::ValidationError(
                "Invalid email: missing @".to_string(),
            ));
        }
        let (local, domain) = (parts[0], parts[1]);
        if local.is_empty() || domain.is_empty() {
            return Err(UserError::ValidationError(
                "Invalid email: empty local part or domain".to_string(),
            ));
        }
        if !domain.contains('.') {
            return Err(UserError::ValidationError(
                "Invalid email: domain must contain a dot".to_string(),
            ));
        }
        // Reject characters commonly used in XSS / header injection
        let forbidden = [
            '<', '>', '"', '\'', '\\', ' ', '\t', '\n', '\r', '(', ')', ',', ';',
        ];
        if email.chars().any(|c| forbidden.contains(&c)) {
            return Err(UserError::ValidationError(
                "Invalid email: contains forbidden characters".to_string(),
            ));
        }
        if email.len() > 254 {
            return Err(UserError::ValidationError(
                "Invalid email: too long (max 254 characters)".to_string(),
            ));
        }
        Ok(())
    }
}
