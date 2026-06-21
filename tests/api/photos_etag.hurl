# =============================================================
# OxiCloud – Photos timeline ETag / 304 conditional revalidation
# =============================================================
# Proves GET /api/photos returns a stable, content-derived ETag and
# honours If-None-Match with an EMPTY 304 — so "navigate away and back"
# to an unchanged gallery skips rebuilding the DTOs + reserializing +
# reshipping the whole page body. Run:
#   hurl --variables-file tests/api/test.env --file-root tests \
#        --test tests/api/setup.hurl tests/api/photos_etag.hurl
# =============================================================


# ─────────────────────────────────────────────────────────────
# Step 1 – Login
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/auth/login
Content-Type: application/json
{
  "username": "{{username}}",
  "password": "{{password}}"
}

HTTP 200
[Captures]
token: jsonpath "$.access_token"


# ─────────────────────────────────────────────────────────────
# Step 2 – Home folder id (upload target)
# ─────────────────────────────────────────────────────────────
GET {{base_url}}/api/folders
Authorization: Bearer {{token}}

HTTP 200
[Captures]
home_folder_id: jsonpath "$[0].id"


# ─────────────────────────────────────────────────────────────
# Step 3 – Upload an image so the photos timeline is non-empty
# ─────────────────────────────────────────────────────────────
POST {{base_url}}/api/files/upload
Authorization: Bearer {{token}}
[MultipartFormData]
folder_id: {{home_folder_id}}
file: file,fixtures/dedup-test.jpg; image/jpeg

HTTP 201


# ─────────────────────────────────────────────────────────────
# Step 4 – First GET: 200 with a body and an ETag (capture it).
#          Cache-Control: no-cache makes the browser revalidate.
# ─────────────────────────────────────────────────────────────
GET {{base_url}}/api/photos?limit=200
Authorization: Bearer {{token}}

HTTP 200
[Captures]
photos_etag: header "ETag"
[Asserts]
header "ETag" exists
header "Cache-Control" contains "no-cache"
jsonpath "$" isCollection
jsonpath "$" count >= 1


# ─────────────────────────────────────────────────────────────
# Step 5 – Conditional GET with the same ETag: empty 304 (the win).
# ─────────────────────────────────────────────────────────────
GET {{base_url}}/api/photos?limit=200
Authorization: Bearer {{token}}
If-None-Match: {{photos_etag}}

HTTP 304
[Asserts]
header "ETag" == "{{photos_etag}}"
bytes count == 0


# ─────────────────────────────────────────────────────────────
# Step 6 – A stale/mismatched ETag still gets the full 200 body.
# ─────────────────────────────────────────────────────────────
GET {{base_url}}/api/photos?limit=200
Authorization: Bearer {{token}}
If-None-Match: "stale-etag-does-not-match"

HTTP 200
[Asserts]
jsonpath "$" count >= 1
