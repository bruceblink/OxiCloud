-- ════════════════════════════════════════════════════════════════════════════
-- Per-user preferred locale (PR C of the i18n / magic-link templating work)
-- ════════════════════════════════════════════════════════════════════════════
-- Carries the user's preference for server-rendered surfaces — chiefly
-- transactional emails (invitation, login-via-email magic-link) and
-- future server-rendered HTML for authenticated users. The frontend
-- language switcher writes here via PATCH /api/auth/me/profile so the
-- choice survives across sessions and devices; the OIDC callback writes
-- here once at JIT provisioning if the IdP's `locale` claim resolves
-- against the LocaleRegistry; the magic-link invitation flow copies the
-- inviter's value into the new external user's row.
--
-- Value semantics:
--   NULL    — no explicit preference. The application resolves to
--             OXICLOUD_DEFAULT_LOCALE (default "en"). NULL is also the
--             post-rollback shape; nothing reads this column in a way
--             that requires it to be set.
--   "xx"    — IETF BCP-47 primary tag, e.g. "en", "fr", "ja".
--   "xx-YY" — primary + region subtag, e.g. "zh-TW".
--
-- The CHECK below enforces a permissive but bounded shape that matches
-- what the LocaleRegistry's case-insensitive comparison will canonicalise
-- successfully. We do NOT enforce membership in the registry's
-- discovered codes at the DB level — that list is build-time runtime
-- state, not schema. The application layer is the gatekeeper:
-- `update_profile_with_perms` rejects unknown codes with 400, and the
-- email-render path silently falls back to the server default when a
-- stored value no longer resolves (e.g. after dropping a locale file).
--
-- No backfill: every existing row stays NULL → inherits the server
-- default, which is the same behaviour every row had before this
-- migration. Pre-PR-C users see no change.

ALTER TABLE auth.users
    ADD COLUMN preferred_locale TEXT NULL
        CHECK (preferred_locale IS NULL
            OR preferred_locale ~ '^[a-zA-Z]{2,3}(-[a-zA-Z0-9]{2,8})*$');

COMMENT ON COLUMN auth.users.preferred_locale IS
    'User-preferred locale for server-rendered surfaces (emails, future
     auth pages). IETF BCP-47 shape: primary tag + optional subtags.
     NULL = no preference; resolves to OXICLOUD_DEFAULT_LOCALE.
     Set by: UI language switcher (PATCH /api/auth/me/profile),
     OIDC JIT provisioning (one-shot, never re-applied on subsequent
     logins — UI choice is canonical), inheritance from inviter at
     external-user creation. Application enforces registry membership;
     schema only constrains the textual shape.';
