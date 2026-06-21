# Photos timeline — ETag / 304 conditional revalidation

`GET /api/photos` returned only `X-Next-Cursor` — no ETag. So every time the
gallery re-mounts (navigate away and back), the server rebuilds up to 500
`PhotoDto`s (each a `FileDto::from` with several allocations), serde-serializes
the whole vector, and ships the full JSON body — even when nothing changed.

The handler now emits a lightweight, content-derived ETag and honours
`If-None-Match`, with `Cache-Control: private, no-cache` so the browser always
revalidates. An unchanged gallery re-mount becomes an empty **304** instead of a
full rebuild + reserialize + re-transfer.

- **ETag** = hash of `(before, limit, max(modified_at), row count)` over the page
  — page identity plus a freshness signal, mirroring the file-list endpoint.
  Any upload/edit/delete that changes the page bumps `max(modified_at)` or the
  count, so the ETag changes and the client gets the fresh body.
- The browser revalidates because the SPA's `apiFetch` uses the default fetch
  cache mode and `no-cache` forces a conditional request.
- The DB query still runs (it's the cheap part); the win is skipping the DTO
  build + serialization + body bytes.

## Reproduce / proof

`tests/api/photos_etag.hurl` (run by the api-test suite) asserts: first GET → 200
+ ETag + `Cache-Control: no-cache`; conditional GET with the matching ETag →
empty 304; a stale `If-None-Match` → full 200. End-to-end measurement against a
throwaway Postgres + server (7 uploaded images):

```
1st GET (no If-None-Match)      : 200  4586 bytes  + ETag
2nd GET (If-None-Match matches) : 304     0 bytes   ← the win
3rd GET (If-None-Match stale)   : 200  4586 bytes   ← correctly invalidated
```

~655 B/photo on the wire. At a full 500-row first page that's **~320 KB +
500-DTO build/serialize saved per unchanged "navigate away and back"**, plus the
response bytes. Steady-state gallery navigation is the common case, so this hits
real user-facing latency (unlike a one-time cold load).
