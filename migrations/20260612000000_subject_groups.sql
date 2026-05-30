-- ─────────────────────────────────────────────────────────────────────────
-- ReBAC Subject Groups — root-owned authorization principals with cascading
-- membership (User ∈ Group, Group ∈ Group).
--
-- Granting permission on a file/folder to a subject_group cascades to every
-- user transitively a member of that group. Cycles are forbidden (checked
-- at write time by application code). Group names follow RFC 5321 local-part
-- shape so future mailing-list addressing (`<name>@instance`) is possible.
--
-- Companion code: src/domain/entities/subject_group.rs and
--                 src/infrastructure/services/pg_acl_engine.rs (expand_subject).
-- ─────────────────────────────────────────────────────────────────────────

-- Case-insensitive uniqueness on group name (handles "Eng" vs "eng" collision).
CREATE EXTENSION IF NOT EXISTS citext;

-- ── auth.subject_groups ─────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS auth.subject_groups (
    id          UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    name        CITEXT       NOT NULL,
    description TEXT,
    is_virtual  BOOLEAN      NOT NULL DEFAULT FALSE,
    created_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),

    -- RFC 5321 local-part: starts alnum, then alnum/dot/dash/underscore,
    -- max 64 chars. Future-proofs `<name>@instance` mailing-list addressing.
    CONSTRAINT subject_groups_name_rfc5321
        CHECK (name ~ '^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$'),
    CONSTRAINT subject_groups_name_uq UNIQUE (name)
);

CREATE INDEX IF NOT EXISTS idx_subject_groups_is_virtual
    ON auth.subject_groups (is_virtual) WHERE is_virtual = TRUE;

-- ── auth.subject_group_members ──────────────────────────────────────────
-- Tagged-union row: exactly one of (member_user_id, member_group_id) is set.
CREATE TABLE IF NOT EXISTS auth.subject_group_members (
    group_id        UUID NOT NULL REFERENCES auth.subject_groups(id) ON DELETE CASCADE,
    member_user_id  UUID         REFERENCES auth.users(id)           ON DELETE CASCADE,
    member_group_id UUID         REFERENCES auth.subject_groups(id)  ON DELETE CASCADE,
    added_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
    added_by        UUID         NOT NULL REFERENCES auth.users(id),

    -- Exactly one of the two member columns is set.
    CONSTRAINT subject_group_members_xor CHECK (
        (member_user_id IS NOT NULL)::int + (member_group_id IS NOT NULL)::int = 1
    ),
    -- A group can't contain itself directly (cycles of length > 1 are
    -- rejected at write time by the application layer's recursive-CTE check).
    CONSTRAINT subject_group_members_no_self CHECK (
        member_group_id IS NULL OR member_group_id <> group_id
    )
);

-- Each (group, user_member) pair unique.
CREATE UNIQUE INDEX IF NOT EXISTS idx_subject_group_members_user
    ON auth.subject_group_members (group_id, member_user_id)
    WHERE member_user_id IS NOT NULL;

-- Each (group, group_member) pair unique.
CREATE UNIQUE INDEX IF NOT EXISTS idx_subject_group_members_group
    ON auth.subject_group_members (group_id, member_group_id)
    WHERE member_group_id IS NOT NULL;

-- Hot path: "all groups a user belongs to directly" (base step of
-- groups_for_user recursive CTE).
CREATE INDEX IF NOT EXISTS idx_subject_group_members_by_user
    ON auth.subject_group_members (member_user_id)
    WHERE member_user_id IS NOT NULL;

-- Cycle / transitive-expansion: "what groups contain group X as a member".
CREATE INDEX IF NOT EXISTS idx_subject_group_members_by_child_group
    ON auth.subject_group_members (member_group_id, group_id)
    WHERE member_group_id IS NOT NULL;

-- ── Seed the predefined `Internal` virtual group ────────────────────────
-- Well-known UUID hard-coded in Rust (`INTERNAL_GROUP_ID` constant in
-- src/domain/entities/subject_group.rs) so application code can reference
-- it without a runtime lookup. Membership is *implicit*: every non-external
-- user is treated as a member by pg_acl_engine::expand_subject; no rows in
-- subject_group_members exist for this group.
INSERT INTO auth.subject_groups (id, name, description, is_virtual)
VALUES (
    '00000000-0000-0000-0000-000000000001',
    'Internal',
    'All internal users (is_external = false). Membership is implicit; no rows in subject_group_members.',
    TRUE
)
ON CONFLICT (id) DO NOTHING;
