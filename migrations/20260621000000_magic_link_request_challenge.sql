-- ════════════════════════════════════════════════════════════════════════════
-- Device-bound magic-link redemption (PR 22)
-- ════════════════════════════════════════════════════════════════════════════
-- Login-via-email tokens (the ones the user requests themselves from their
-- own browser) now carry a per-request challenge that mirrors a cookie
-- set on the originating browser. On redemption the server compares the
-- inbound cookie against this column:
--
--   - Cookie present and matches → redeem instantly (common case, zero
--     UX change for the user clicking from the same browser).
--   - Cookie absent or mismatched → show a confirmation page; user
--     clicks Continue to redeem anyway. Audit-logged as
--     `cross_browser_confirmed`.
--
-- Invitation tokens (the ones a sharer mints for a recipient who has no
-- prior browser context with the server) leave this column NULL — they
-- are cross-device by design and bypass the cookie check entirely.
--
-- See docs/architecture/magic-link-auth.md and auth-simplification.md
-- (PR 22) for the threat model and full design.

ALTER TABLE auth.magic_link_tokens
    ADD COLUMN request_challenge TEXT NULL;

COMMENT ON COLUMN auth.magic_link_tokens.request_challenge IS
    'Random per-request value mirrored into the oxicloud_magic_request
     cookie on the originating browser. NULL for invitation tokens
     (cross-device by design); non-NULL for login-via-email tokens
     (browser-bound). Compared on redemption to bind the magic-link to
     the device that requested it.';
