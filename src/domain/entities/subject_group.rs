//! Subject group: ReBAC authorization principal.
//!
//! Subject groups are root-owned (no `owner_id`), globally named with an
//! RFC 5321 local-part shape, and able to contain users *or* other groups.
//! Grants in `storage.access_grants` with `subject_type = 'group'` reference
//! a row in `auth.subject_groups`.
//!
//! Cycle prevention and depth-cap (`MAX_GROUP_DEPTH`) are enforced at the
//! application layer at write time. The database schema enforces:
//!   - case-insensitive uniqueness on `name` (CITEXT),
//!   - the RFC 5321 local-part shape (CHECK regex),
//!   - the XOR of (`member_user_id`, `member_group_id`) on memberships,
//!   - no self-membership at the row level (a group can't list itself
//!     directly as a child — longer cycles are application-layer concerns).
//!
//! See `migrations/20260612000000_subject_groups.sql`.

use chrono::{DateTime, Utc};
use uuid::Uuid;

pub use super::entity_errors::{SubjectGroupError, SubjectGroupResult};

/// Well-known UUID of the predefined `Internal` virtual group.
///
/// Hard-coded so application code can reference it without a runtime
/// lookup. Membership is implicit: every authenticated user is treated as
/// belonging to this group at evaluation time (see
/// `PgAclEngine::expand_subject`). Once the external-users work lands, this
/// will narrow to "every user with `is_external = false`".
pub const INTERNAL_GROUP_ID: Uuid =
    Uuid::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

/// Maximum allowed nesting depth for groups-of-groups.
///
/// Enforced at write time inside `add_member`. The recursive CTE that
/// expands a user's transitive membership is bounded by this value, which
/// keeps authz checks predictable and prevents pathological graphs.
pub const MAX_GROUP_DEPTH: u8 = 8;

/// Maximum length of an RFC 5321 local-part.
const MAX_NAME_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectGroup {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub is_virtual: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A member of a subject group: either an internal user or another group.
///
/// Externals and tokens may be added later; today only users and groups can
/// be members (matching the schema's tagged-union row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupMember {
    User(Uuid),
    Group(Uuid),
}

impl SubjectGroup {
    /// Construct a new group, validating the name shape.
    ///
    /// The DB CHECK constraint is the authority; this validation exists so
    /// the service layer can return a typed error before the round-trip.
    pub fn new(name: &str, description: Option<String>) -> SubjectGroupResult<Self> {
        Self::validate_name(name)?;
        let now = Utc::now();
        Ok(Self {
            id: Uuid::new_v4(),
            name: name.to_string(),
            description,
            is_virtual: false,
            created_at: now,
            updated_at: now,
        })
    }

    /// Validate the name against the RFC 5321 local-part shape used by the
    /// DB CHECK constraint: `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`.
    pub fn validate_name(name: &str) -> SubjectGroupResult<()> {
        if name.is_empty() {
            return Err(SubjectGroupError::InvalidName("empty".to_string()));
        }
        if name.len() > MAX_NAME_LEN {
            return Err(SubjectGroupError::InvalidName(format!(
                "exceeds {} chars",
                MAX_NAME_LEN
            )));
        }

        // First char: must be alphanumeric ASCII (RFC 5321 is ASCII-only).
        let mut chars = name.chars();
        let first = chars.next().expect("non-empty above");
        if !first.is_ascii_alphanumeric() {
            return Err(SubjectGroupError::InvalidName(format!(
                "must start with letter or digit: {}",
                name
            )));
        }

        // Remaining: alphanumeric or one of `.`, `-`, `_`.
        for c in chars {
            if !(c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_') {
                return Err(SubjectGroupError::InvalidName(format!(
                    "invalid character {:?} in {}",
                    c, name
                )));
            }
        }

        Ok(())
    }

    /// Whether mutations on this group are forbidden by virtue of it being a
    /// system-managed virtual group (e.g. `Internal`).
    pub fn is_immutable(&self) -> bool {
        self.is_virtual
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_simple_rfc5321_local_part() {
        assert!(SubjectGroup::validate_name("engineering").is_ok());
        assert!(SubjectGroup::validate_name("eng-team_42").is_ok());
        assert!(SubjectGroup::validate_name("a.b.c").is_ok());
        assert!(SubjectGroup::validate_name("X").is_ok());
    }

    #[test]
    fn rejects_empty_name() {
        assert!(matches!(
            SubjectGroup::validate_name(""),
            Err(SubjectGroupError::InvalidName(_))
        ));
    }

    #[test]
    fn rejects_space() {
        assert!(matches!(
            SubjectGroup::validate_name("Engineering Team"),
            Err(SubjectGroupError::InvalidName(_))
        ));
    }

    #[test]
    fn rejects_non_alnum_start() {
        assert!(matches!(
            SubjectGroup::validate_name(".dotfirst"),
            Err(SubjectGroupError::InvalidName(_))
        ));
        assert!(matches!(
            SubjectGroup::validate_name("-dashfirst"),
            Err(SubjectGroupError::InvalidName(_))
        ));
    }

    #[test]
    fn rejects_unicode() {
        assert!(matches!(
            SubjectGroup::validate_name("équipe"),
            Err(SubjectGroupError::InvalidName(_))
        ));
        assert!(matches!(
            SubjectGroup::validate_name("group🚀"),
            Err(SubjectGroupError::InvalidName(_))
        ));
    }

    #[test]
    fn rejects_too_long() {
        let name = "a".repeat(65);
        assert!(matches!(
            SubjectGroup::validate_name(&name),
            Err(SubjectGroupError::InvalidName(_))
        ));
    }

    #[test]
    fn accepts_exactly_64_chars() {
        let name = "a".repeat(64);
        assert!(SubjectGroup::validate_name(&name).is_ok());
    }

    #[test]
    fn internal_group_id_is_stable() {
        // Match the well-known UUID seeded by migration 20260612000000.
        assert_eq!(
            INTERNAL_GROUP_ID.to_string(),
            "00000000-0000-0000-0000-000000000001"
        );
    }
}
