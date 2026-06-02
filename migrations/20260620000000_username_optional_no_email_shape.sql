-- ════════════════════════════════════════════════════════════════════════════
-- Auth simplification — username + password_hash become nullable
-- ════════════════════════════════════════════════════════════════════════════
-- This migration completes the auth-simplification design (PR 16 of the
-- auth-simplification plan):
--
--   * `username` becomes NULLABLE. External users get NULL; internal users
--     keep their handles. Multiple NULLs coexist under the existing UNIQUE
--     index (Postgres allows this by default).
--   * `password_hash` becomes NULLABLE. The sentinel strings
--     `__EXTERNAL_NO_PASSWORD__` and `__OIDC_NO_PASSWORD__` are NULL'd out;
--     the entity-level checks switch from string comparison to
--     `Option::is_some`.
--   * Username format CHECK tightens: 2-64 chars, no `@` (banning `@`
--     keeps the username and email namespaces provably disjoint and
--     prevents the cross-collision attack class described in the
--     auth-simplification plan).
--
-- Forward-only — do NOT squash with `20260612000003_users_username_email_login.sql`.
-- That migration has already been applied to dev / CI environments;
-- squashing would invalidate `_sqlx_migrations` checksums and lock down
-- the migration runner.

-- 1. Drop NOT NULL on the two columns we're loosening.
ALTER TABLE auth.users ALTER COLUMN username      DROP NOT NULL;
ALTER TABLE auth.users ALTER COLUMN password_hash DROP NOT NULL;

-- 2. NULL out the email-shaped usernames that PR 9 stamped onto external
--    users. Their identity is the email column; the username field carried
--    a redundant duplicate that was only ever used to satisfy NOT NULL.
UPDATE auth.users
   SET username = NULL
 WHERE is_external = TRUE;

-- 3. NULL out the placeholder password_hash sentinels. After this migration
--    `password_hash IS NULL` means "no password set"; non-NULL means
--    "argon2 hash". No more string-comparison gymnastics in the entity.
UPDATE auth.users
   SET password_hash = NULL
 WHERE password_hash IN ('__EXTERNAL_NO_PASSWORD__', '__OIDC_NO_PASSWORD__');

-- 4. Tighten username format. The CHECK fires only when username IS NOT
--    NULL (existing externals stay NULL; new email-shaped values are
--    rejected at write time). Length 2-64 matches the entity validator's
--    new range. Existing internal usernames are all ≥3 and ≤32 chars,
--    so this is non-breaking for current data.
ALTER TABLE auth.users
    ADD CONSTRAINT users_username_shape_v2
    CHECK (username IS NULL
        OR (username !~ '@' AND char_length(username) BETWEEN 2 AND 64));

COMMENT ON COLUMN auth.users.username IS
    'Optional handle (2-64 chars, no `@`). NULL for external users and for users who haven''t claimed one yet. UNIQUE allows multiple NULLs by default.';

COMMENT ON COLUMN auth.users.password_hash IS
    'Argon2 password hash. NULL when the user has no password (externals, OIDC-only users, or post-PR-18 email-only signups awaiting their welcome magic-link).';
