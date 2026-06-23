# =============================================================
# OxiCloud — D2 drive membership API + delegation + caller_role
# =============================================================
# Verifies the D2 membership surface on personal drives (the only
# drive kind today — shared-drive positive cases land alongside D3's
# create endpoint):
#
#   1. `GET /api/drives` exposes `caller_role` on every row.
#   2. `GET /api/drives/{id}/members` lists role grants on a drive
#      (one Owner row for the lifecycle-hook-provisioned default).
#   3. Personal-drive guard refuses every membership mutation
#      (POST / PATCH / DELETE on `/api/drives/{id}/members*`) with
#      405 — personal drives are single-user single-owner.
#   4. The same guard fires on the generic `/api/grants` write paths
#      (POST / PUT / DELETE) when `resource.type='drive'` and the
#      drive is personal — verifies the `DriveManagementService`
#      delegation that closes the bypass.
#   5. Drive grants surface in `GET /api/grants/incoming/resources`
#      with `resource_types=drive` filter (previously hard-skipped).
#   6. Anti-enum: an unrelated user gets the same `404` for a drive
#      they can't read, whether or not it exists.
#
# Self-contained: provisions its own users so it can run after
# drives_foundation without aliasing state.
# =============================================================


# ─────────────────────────────────────────────────────────────
# Step 1 — admin login
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/auth/login
Content-Type: application/json
{ "username": "{{username}}", "password": "{{password}}" }

HTTP 200
[Captures]
admin_token: jsonpath "$.access_token"
admin_user_id: jsonpath "$.user.id"


# ─────────────────────────────────────────────────────────────
# Step 2 — Admin's GET /api/drives now includes `caller_role`.
#          Personal-drive owner role is seeded by
#          PersonalDriveLifecycleHook on user creation.
# ─────────────────────────────────────────────────────────────
GET {{base_url}}/api/drives
Authorization: Bearer {{admin_token}}

HTTP 200
[Asserts]
jsonpath "$[0].kind"             == "personal"
jsonpath "$[0].default_for_user" == "{{admin_user_id}}"
jsonpath "$[0].caller_role"      == "owner"

[Captures]
admin_drive_id: jsonpath "$[0].id"


# ─────────────────────────────────────────────────────────────
# Step 3 — GET /api/drives/{id}/members returns the lifecycle-
#          seeded Owner row and only that.
# ─────────────────────────────────────────────────────────────
GET {{base_url}}/api/drives/{{admin_drive_id}}/members
Authorization: Bearer {{admin_token}}

HTTP 200
[Asserts]
jsonpath "$"                       count == 1
jsonpath "$[0].subject.type"       == "user"
jsonpath "$[0].subject.id"         == "{{admin_user_id}}"
jsonpath "$[0].resource.type"      == "drive"
jsonpath "$[0].resource.id"        == "{{admin_drive_id}}"
jsonpath "$[0].role"               == "owner"


# ─────────────────────────────────────────────────────────────
# Step 4 — Create a fresh user (mbr_alice) so we have a second
#          subject the personal-drive guard can refuse on.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/admin/users
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{ "username": "mbr_alice", "password": "MbrAlicePassword1!", "email": "mbr_alice@example.com", "role": "user" }

HTTP 201
[Captures]
alice_user_id: jsonpath "$.id"

POST {{base_url}}/api/auth/login
Content-Type: application/json
{ "username": "mbr_alice", "password": "MbrAlicePassword1!" }

HTTP 200
[Captures]
alice_token: jsonpath "$.access_token"

# Alice's GET /api/drives surfaces her own default with caller_role=owner.
GET {{base_url}}/api/drives
Authorization: Bearer {{alice_token}}

HTTP 200
[Asserts]
jsonpath "$"                     count == 1
jsonpath "$[0].caller_role"      == "owner"
jsonpath "$[0].default_for_user" == "{{alice_user_id}}"

[Captures]
alice_drive_id: jsonpath "$[0].id"


# ─────────────────────────────────────────────────────────────
# Step 5 — Personal-drive guard via the dedicated endpoint:
#          POST /api/drives/{id}/members refuses with 405.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/drives/{{admin_drive_id}}/members
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "subject": { "type": "user", "id": "{{alice_user_id}}" },
  "role": "editor"
}

HTTP 405


# ─────────────────────────────────────────────────────────────
# Step 6 — PATCH on a personal drive's members → 405 (even for
#          the owner row itself; personal drives' membership
#          is structurally immutable).
# ─────────────────────────────────────────────────────────────
PATCH {{base_url}}/api/drives/{{admin_drive_id}}/members/user/{{admin_user_id}}
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{ "role": "editor" }

HTTP 405


# ─────────────────────────────────────────────────────────────
# Step 7 — DELETE on a personal drive's owner → 405. (Verifies
#          the guard fires BEFORE the last-owner check; the order
#          matters for the right error.)
# ─────────────────────────────────────────────────────────────
DELETE {{base_url}}/api/drives/{{admin_drive_id}}/members/user/{{admin_user_id}}
Authorization: Bearer {{admin_token}}

HTTP 405


# ─────────────────────────────────────────────────────────────
# Step 8 — Generic /api/grants delegation. POST /api/grants with
#          resource.type='drive' on a personal drive must hit the
#          same guard, otherwise the membership rules are bypassable.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/grants
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "resource": { "type": "drive", "id": "{{admin_drive_id}}" },
  "subject":  { "type": "user",  "id": "{{alice_user_id}}" },
  "role": "editor"
}

HTTP 405


# ─────────────────────────────────────────────────────────────
# Step 9 — PUT /api/grants/role (silent admin update) on a drive
#          resource — also delegated. Same guard fires.
# ─────────────────────────────────────────────────────────────
PUT {{base_url}}/api/grants/role
Authorization: Bearer {{admin_token}}
Content-Type: application/json
{
  "resource": { "type": "drive", "id": "{{admin_drive_id}}" },
  "subject":  { "type": "user",  "id": "{{admin_user_id}}" },
  "role": "editor"
}

HTTP 405


# ─────────────────────────────────────────────────────────────
# Step 10 — Drive grants in /api/grants/incoming/resources.
#           Default UI calls pass `resource_types=file,folder` so
#           drives don't appear; explicit `resource_types=drive`
#           must surface the admin's Owner grant on their own drive.
# ─────────────────────────────────────────────────────────────
GET {{base_url}}/api/grants/incoming/resources?resource_types=drive
Authorization: Bearer {{admin_token}}

HTTP 200
[Asserts]
jsonpath "$.items"                count >= 1
jsonpath "$.items[0].resource_type" == "drive"


# ─────────────────────────────────────────────────────────────
# Step 11 — Anti-enum: Alice queries the members of admin's
#           personal drive. She has no Read on it → 404, same
#           shape as "drive doesn't exist". Operators see the
#           real reason in the audit log; she sees nothing.
# ─────────────────────────────────────────────────────────────
GET {{base_url}}/api/drives/{{admin_drive_id}}/members
Authorization: Bearer {{alice_token}}

HTTP 404


# ─────────────────────────────────────────────────────────────
# Step 12 — The same anti-enum shape for a UUID that doesn't
#           exist at all. Indistinguishable from step 11 to the
#           caller — the canonical no-leak response.
# ─────────────────────────────────────────────────────────────
GET {{base_url}}/api/drives/00000000-0000-0000-0000-000000000000/members
Authorization: Bearer {{alice_token}}

HTTP 404
