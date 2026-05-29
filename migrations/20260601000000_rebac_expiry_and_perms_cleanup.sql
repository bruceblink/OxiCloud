-- ════════════════════════════════════════════════════════════════════════════
-- ReBAC Phase 2: grant-level expiry + dead permission column cleanup
-- ════════════════════════════════════════════════════════════════════════════
-- This migration:
--   1. Adds expires_at (TIMESTAMPTZ) to access_grants — uniform expiry for
--      all subject types (token, user, future external).
--   2. Migrates existing token expiry from storage.shares.expires_at.
--   3. Backfills any Read grants missing for shares created after the
--      initial migration (safety net — idempotent via NOT EXISTS).
--   4. Adds two performance indexes (expires_at partial, granted_by).
--   5. Drops the now-dead permission and expiry columns from storage.shares.
--      storage.shares becomes token-only metadata: id, token, password_hash,
--      access_count, created_at, created_by, item_id, item_type, item_name.
--
-- Conceptual model: a share token is an authentication principal, not a
-- permission type. Access = having a non-expired Read grant in access_grants
-- for Subject::Token(share.id). Tokens are always read-only by definition.

-- ── 1. Add expires_at ────────────────────────────────────────────────────────
ALTER TABLE storage.access_grants
    ADD COLUMN IF NOT EXISTS expires_at TIMESTAMPTZ;

-- ── 2. Migrate token expiry (shares.expires_at is BIGINT unix seconds) ───────
UPDATE storage.access_grants ag
SET    expires_at = to_timestamp(s.expires_at)
FROM   storage.shares s
WHERE  ag.subject_type = 'token'
  AND  ag.subject_id   = s.id
  AND  s.expires_at IS NOT NULL;

-- ── 3. Backfill Read grants for shares that missed the initial migration ──────
INSERT INTO storage.access_grants
    (subject_type, subject_id, resource_type, resource_id, permission, granted_by, granted_at)
SELECT
    'token',
    s.id,
    s.item_type,
    s.item_id::UUID,
    'read',
    s.created_by,
    to_timestamp(s.created_at)
FROM storage.shares s
WHERE s.permissions_read
  AND NOT EXISTS (
      SELECT 1 FROM storage.access_grants ag
      WHERE ag.subject_type = 'token'
        AND ag.subject_id   = s.id
        AND ag.permission   = 'read'
  )
ON CONFLICT DO NOTHING;

-- ── 4. Performance indexes ───────────────────────────────────────────────────
-- Partial index for expiry checks (only rows that actually expire)
CREATE INDEX IF NOT EXISTS idx_grants_expires_at
    ON storage.access_grants (expires_at) WHERE expires_at IS NOT NULL;

-- Needed for GET /api/grants/outgoing/resources (currently missing)
CREATE INDEX IF NOT EXISTS idx_grants_granted_by
    ON storage.access_grants (granted_by);

-- ── 5. Drop dead columns from storage.shares ─────────────────────────────────
-- Permissions were never enforced (no public write endpoints, frontend
-- hard-codes write=false/reshare=false). Expiry is now in access_grants.
ALTER TABLE storage.shares
    DROP COLUMN IF EXISTS permissions_read,
    DROP COLUMN IF EXISTS permissions_write,
    DROP COLUMN IF EXISTS permissions_reshare,
    DROP COLUMN IF EXISTS expires_at;
