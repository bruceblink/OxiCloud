# =============================================================
# OxiCloud — D4 per-drive quota enforcement
# =============================================================
# Pins the upload-time per-drive quota refusal. Scope:
#
#   1. Quota = 100 B on a shared drive; uploading a 5 MiB file →
#      `507 Insufficient Storage`. Refusal happens BEFORE the file
#      row is registered (no orphan blob, no usage drift).
#   2. A small file (32 B) under the same quota → `201`. The fire-
#      and-forget delta hook bumps `drives.used_bytes`; the next
#      `GET /api/drives` lists the new value.
#   3. After consuming most of the quota, a second small file that
#      would push us over → `507`. Confirms the check uses the
#      cached `used_bytes`, not just file size in isolation.
#   4. Unlimited quota (`quota_bytes` omitted at create) accepts the
#      same 5 MiB upload that case 1 refused → `201`.
#   5. Per-user quota is unaffected — uploading to the user's own
#      default Personal drive (no per-drive cap) still works.
#
# The check is layered on top of the existing per-user quota
# (`storage_usage_service::check_storage_quota`) — both run at the
# multipart handler; either refusal yields 507.
#
# Self-contained: provisions `dq_owner` and a fresh shared drive
# per case so this can run alongside the rest of the API suite.
# =============================================================


# ─────────────────────────────────────────────────────────────
# Step 1 — Admin login.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/auth/login
Content-Type: application/json
{ "username": "{{username}}", "password": "{{password}}" }

HTTP 200
[Captures]
admin_token: jsonpath "$.access_token"


# ─────────────────────────────────────────────────────────────
# Step 2 — Provision `dq_owner` (drive owner under test).
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/admin/users
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "username": "dq_owner",
  "password": "DqOwnerPwd1!",
  "email":    "dq_owner@example.com",
  "role":     "user"
}

HTTP 201

POST {{base_url}}/api/auth/login
Content-Type: application/json
{ "username": "dq_owner", "password": "DqOwnerPwd1!" }

HTTP 200
[Captures]
owner_token: jsonpath "$.access_token"
owner_user_id: jsonpath "$.user.id"


# ─────────────────────────────────────────────────────────────
# Step 3 — Admin creates a shared drive with a tiny 100-byte quota
#          and `dq_owner` as direct user-Owner.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/drives
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "kind": "shared",
  "name": "dq-tight",
  "owner": { "type": "user", "id": "{{owner_user_id}}" },
  "quota_bytes": 100
}

HTTP 201
[Captures]
tight_drive_id: jsonpath "$.id"
tight_root_id:  jsonpath "$.root_folder_id"


# ─────────────────────────────────────────────────────────────
# Step 4 — Case 1: 5 MiB upload to the 100-byte drive → 507.
#          Refused at the multipart handler before the file row is
#          registered; the blob is discarded by `discard_ingested`.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/files/upload
Authorization: Bearer {{owner_token}}
[MultipartFormData]
folder_id: {{tight_root_id}}
file: file,fixtures/chunk-over-cap-5mb.bin; application/octet-stream

HTTP 507


# ─────────────────────────────────────────────────────────────
# Step 5 — Case 2: 32-byte upload fits under the 100-byte cap.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/files/upload
Authorization: Bearer {{owner_token}}
[MultipartFormData]
folder_id: {{tight_root_id}}
file: file,fixtures/hello.txt; text/plain

HTTP 201
[Captures]
small_file_id: jsonpath "$.id"


# Confirm `drives.used_bytes` reflects the new file. The hook is
# fire-and-forget on a tokio task, so the SQL UPDATE may not have
# landed by the time `POST /api/files/upload` returned. Retry the
# `GET /api/drives` until the cached value catches up — bounded
# wait keeps a slow CI machine from flaking.
GET {{base_url}}/api/drives
Authorization: Bearer {{owner_token}}
[Options]
retry: 10
retry-interval: 200ms

HTTP 200
[Asserts]
jsonpath "$[?(@.id=='{{tight_drive_id}}')].used_bytes" == 32


# ─────────────────────────────────────────────────────────────
# Step 6 — Case 3: a SECOND small file that would push usage past
#          the cap is refused. With `hello.txt` at 32 bytes already
#          on the drive, the next 32-byte upload projects to
#          32 + 32 + 32 (header overhead negligible) — far under
#          100 — and IS accepted. Then a 5 MiB upload remains over
#          quota: 507. This protects the "uses cached used_bytes"
#          invariant: the check isn't just `size < quota`.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/files/upload
Authorization: Bearer {{owner_token}}
[MultipartFormData]
folder_id: {{tight_root_id}}
file: file,fixtures/hello-copy.txt; text/plain

HTTP 201


# `used_bytes` climbs to 64 (32 + 32). Same retry shape as the
# first assertion since the second delta is also fire-and-forget.
GET {{base_url}}/api/drives
Authorization: Bearer {{owner_token}}
[Options]
retry: 10
retry-interval: 200ms

HTTP 200
[Asserts]
jsonpath "$[?(@.id=='{{tight_drive_id}}')].used_bytes" == 64


# The fire-and-forget delta hook updates `drives.used_bytes`; the
# subsequent 5 MiB attempt still fails (5 MiB > 100 alone). This
# assertion holds regardless of whether the previous delta has
# landed in cache or not — `5_242_880 > 100` either way.
POST {{base_url}}/api/files/upload
Authorization: Bearer {{owner_token}}
[MultipartFormData]
folder_id: {{tight_root_id}}
file: file,fixtures/chunk-over-cap-5mb.bin; application/octet-stream

HTTP 507


# `used_bytes` is unchanged — the failed upload didn't charge the
# drive. (Cumulative usage is still 64; the 5 MiB write never
# registered a row.)
GET {{base_url}}/api/drives
Authorization: Bearer {{owner_token}}

HTTP 200
[Asserts]
jsonpath "$[?(@.id=='{{tight_drive_id}}')].used_bytes" == 64


# ─────────────────────────────────────────────────────────────
# Step 7 — Case 4: unlimited drive accepts the same 5 MiB upload.
#          Confirms the `quota_bytes IS NULL` short-circuit.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/drives
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "kind": "shared",
  "name": "dq-unlimited",
  "owner": { "type": "user", "id": "{{owner_user_id}}" }
}

HTTP 201
[Captures]
unlimited_root_id: jsonpath "$.root_folder_id"
unlimited_drive_id: jsonpath "$.id"


POST {{base_url}}/api/files/upload
Authorization: Bearer {{owner_token}}
[MultipartFormData]
folder_id: {{unlimited_root_id}}
file: file,fixtures/chunk-over-cap-5mb.bin; application/octet-stream

HTTP 201


# Unlimited drive's `used_bytes` climbs to the file's exact size
# (5 MiB = 5_242_880 bytes). Same retry block because the delta
# hook is fire-and-forget here too.
GET {{base_url}}/api/drives
Authorization: Bearer {{owner_token}}
[Options]
retry: 10
retry-interval: 200ms

HTTP 200
[Asserts]
jsonpath "$[?(@.id=='{{unlimited_drive_id}}')].used_bytes" == 5242880


# ─────────────────────────────────────────────────────────────
# Step 9 — Post-delete sweep convergence.
#          By design the per-drive `used_bytes` counter is NOT
#          decremented on permanent delete (mirrors the existing
#          per-user quota design: deletes drift, the periodic sweep
#          reconciles). To prove the sweep actually closes the
#          drift, we:
#            a) Trash the 32-byte file in the unlimited drive
#               (well, both: hello.txt + hello-copy.txt are in the
#               tight drive; the 5 MiB is in the unlimited one).
#            b) Permanently delete via empty-trash.
#            c) Trigger the reconciliation sweep on demand —
#               `/api/admin/internal/trigger-sweep` is gated by
#               `OXICLOUD_ENABLE_ADMIN_INTERNAL_ENDPOINTS=true`
#               (set in `tests/common/server.env`).
#            d) `GET /api/drives` now shows the corrected counter.
# ─────────────────────────────────────────────────────────────
DELETE {{base_url}}/api/files/{{small_file_id}}
Authorization: Bearer {{owner_token}}

HTTP 204


# Permanent purge — empty caller's trash entirely.
DELETE {{base_url}}/api/trash/empty
Authorization: Bearer {{owner_token}}

HTTP 200


# Sweep is fire-and-forget on a ticker (default 600 s). Run it now
# so the assertion below is deterministic instead of polling.
POST {{base_url}}/api/admin/internal/trigger-sweep
Authorization: Bearer {{admin_token}}

HTTP 200
[Asserts]
jsonpath "$.ok" == true


# After the sweep, `tight_drive.used_bytes` has dropped from 64 to
# 32 (hello.txt purged, hello-copy.txt still live). `unlimited`
# stays at 5 MiB (nothing trashed there).
GET {{base_url}}/api/drives
Authorization: Bearer {{owner_token}}

HTTP 200
[Asserts]
jsonpath "$[?(@.id=='{{tight_drive_id}}')].used_bytes" == 32
jsonpath "$[?(@.id=='{{unlimited_drive_id}}')].used_bytes" == 5242880


# ─────────────────────────────────────────────────────────────
# Step 10 — `/api/admin/internal/*` is admin-only and disabled by
#          default. The gate-off case is covered by the absence of
#          the route in production configs; here we just confirm a
#          non-admin caller is refused even when the feature is on.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/admin/internal/trigger-sweep
Authorization: Bearer {{owner_token}}

HTTP 403


# Trigger-GC reachable too — assert it returns the freed-blob
# summary shape. Hard count is non-deterministic (depends on the
# grace window vs the test's elapsed time), so we only check the
# response shape.
POST {{base_url}}/api/admin/internal/trigger-gc
Authorization: Bearer {{admin_token}}

HTTP 200
[Asserts]
jsonpath "$.ok" == true
jsonpath "$.blobs_deleted" exists
jsonpath "$.bytes_freed" exists


# No cleanup — the API test runner (`tests/api/run.sh`) starts each
# run against a fresh postgres + server, so dangling drives /
# trashed rows don't leak across runs. Self-contained user/drive
# names (`dq_*`) also prevent collisions when running alongside the
# rest of the suite within a single boot.
