# =============================================================
# OxiCloud — email-only registration (PR 18)
# =============================================================
# PR 18 makes `password` (and `username`) optional in
# `POST /api/auth/register`. Email-only signup:
#   - returns a uniform 200 message (no JWT, no UserDto)
#   - mints a welcome magic-link mailed to `email`
#   - redemption lands the new internal user on `/#/files`
#     (not `/#/sharedwithme`, which is for externals)
#
# Requires `OXICLOUD_SMTP_MOCK=true` (set in tests/common/server.env).
# =============================================================


# ─────────────────────────────────────────────────────────────
# Step 1 — admin login (cleanup ops at the end need her token).
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/auth/login
Content-Type: application/json
{ "username": "{{username}}", "password": "{{password}}" }

HTTP 200
[Captures]
alice_token: jsonpath "$.access_token"


# ─────────────────────────────────────────────────────────────
# Step 2 — Classic registration (with password). PR 20 anti-
#          enumeration mode (SMTP wired) returns a uniform 200
#          regardless of success or collision. No UserDto in
#          the response — the frontend logs the user in
#          separately to get a session.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/auth/register
Content-Type: application/json
{
  "username": "charlie",
  "email": "charlie@example.com",
  "password": "TestPassword1!"
}

HTTP 200
[Asserts]
jsonpath "$.message" contains "request received"


# ─────────────────────────────────────────────────────────────
# Step 2b — Log in as charlie to confirm registration succeeded
#           AND to capture her user_id for cleanup.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/auth/login
Content-Type: application/json
{ "username": "charlie", "password": "TestPassword1!" }

HTTP 200
[Captures]
charlie_token: jsonpath "$.access_token"
charlie_user_id: jsonpath "$.user.id"


# ─────────────────────────────────────────────────────────────
# Step 3 — Email-only registration. No username, no password.
#          Returns 200 + uniform message; welcome magic-link
#          is captured by the MockEmailSender.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/auth/register
Content-Type: application/json
{
  "email": "pr18-emailonly@example.com"
}

HTTP 200
[Captures]
# PR 22 — capture the browser-binding cookie so the redemption can
# replay it. Hurl's automatic cookie jar doesn't reliably attach
# Path-scoped cookies in this test setup, so we wire it through
# explicitly via the Set-Cookie header.
pr18_magic_cookie: header "set-cookie" regex "oxicloud_magic_request=([^;]+)"
[Asserts]
jsonpath "$.message" contains "request received"


# ─────────────────────────────────────────────────────────────
# Step 4 — Capture the welcome mail + extract the magic-link.
# ─────────────────────────────────────────────────────────────
GET {{base_url}}/api/admin/smtp/test/captured?to=pr18-emailonly@example.com
Authorization: Bearer {{alice_token}}

HTTP 200
[Asserts]
jsonpath "$.to" == "pr18-emailonly@example.com"
jsonpath "$.text_body" matches "/magic/v1/[A-Za-z0-9_-]+"
[Captures]
pr18_magic_url: jsonpath "$.text_body" regex "(https?://[^\\s]+/magic/v1/[A-Za-z0-9_-]+)"


# ─────────────────────────────────────────────────────────────
# Step 5a — Redeem the welcome link WITHOUT the browser-binding
#           cookie. PR 22 shows the cross-browser confirmation
#           page (HTTP 200, HTML) rather than redeeming.
# ─────────────────────────────────────────────────────────────
GET {{pr18_magic_url}}

HTTP 200
[Asserts]
header "content-type" startsWith "text/html"
body contains "different browser"


# ─────────────────────────────────────────────────────────────
# Step 5b — Same link, this time with the matching cookie.
#           PR 22 binds the magic-link to the requesting browser;
#           a matching cookie redeems instantly. Internal user
#           with no resource target → lands on `/#/files`.
# ─────────────────────────────────────────────────────────────
GET {{pr18_magic_url}}
Cookie: oxicloud_magic_request={{pr18_magic_cookie}}

HTTP 302
[Asserts]
header "Location" == "/#/files"
[Captures]
pr18_access_token: cookie "oxicloud_access"


# ─────────────────────────────────────────────────────────────
# Step 6 — The new user can read their own profile. After PR 18
#          the username field is omitted (no handle claimed yet),
#          and `is_external` is false (they're an internal user
#          who signed up directly, not via invitation).
# ─────────────────────────────────────────────────────────────
GET {{base_url}}/api/auth/me
Authorization: Bearer {{pr18_access_token}}

HTTP 200
[Asserts]
jsonpath "$.email" == "pr18-emailonly@example.com"
jsonpath "$.is_external" == false
jsonpath "$.username" not exists
[Captures]
pr18_user_id: jsonpath "$.id"


# ─────────────────────────────────────────────────────────────
# Step 7 — The new user can request another magic-link (no
#          password configured → eligible). Anti-enumeration
#          200 either way.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/auth/magic-link/send
Content-Type: application/json
{ "email": "pr18-emailonly@example.com" }

HTTP 200
[Asserts]
jsonpath "$.message" contains "sign-in link"


# ─────────────────────────────────────────────────────────────
# Step 8 — PR 20 anti-enumeration: register with charlie's
#          email AGAIN (different password). Response is the
#          same uniform 200 — attacker can't tell from the
#          HTTP shape whether the email was already taken.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/auth/register
Content-Type: application/json
{
  "username": "charlie-imposter",
  "email": "charlie@example.com",
  "password": "AttackerPassword99!"
}

HTTP 200
[Asserts]
jsonpath "$.message" contains "request received"


# ─────────────────────────────────────────────────────────────
# Step 9 — Verify the collision was silently suppressed: the
#          attacker's password does NOT work (the original
#          row is intact, no rewrite happened).
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/auth/login
Content-Type: application/json
{ "username": "charlie@example.com", "password": "AttackerPassword99!" }

HTTP 403


# ─────────────────────────────────────────────────────────────
# Step 10 — Charlie's original password still works — the
#           collision didn't touch her account.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/auth/login
Content-Type: application/json
{ "username": "charlie", "password": "TestPassword1!" }

HTTP 200


# ─────────────────────────────────────────────────────────────
# Step 11 — Username collision (different email): same uniform
#           200, no new user, audit `username_taken`.
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/auth/register
Content-Type: application/json
{
  "username": "charlie",
  "email": "charlie-other@example.com",
  "password": "AttackerPassword99!"
}

HTTP 200
[Asserts]
jsonpath "$.message" contains "request received"


# ─────────────────────────────────────────────────────────────
# Cleanup — admin deletes both test users.
# ─────────────────────────────────────────────────────────────
DELETE {{base_url}}/api/admin/users/{{charlie_user_id}}
Authorization: Bearer {{alice_token}}

HTTP *

DELETE {{base_url}}/api/admin/users/{{pr18_user_id}}
Authorization: Bearer {{alice_token}}

HTTP *
