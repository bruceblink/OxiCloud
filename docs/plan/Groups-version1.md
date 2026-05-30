# Plan — ReBAC Subject Groups (v1)

## Context

OxiCloud's `storage.access_grants` table already declares `subject_type IN
('user', 'group', 'token', 'external')` and `expires_at TIMESTAMPTZ`, but no
schema, code, or UI exists today for the `'group'` subject. This plan
implements that: a nested, root-owned group subject with cascading
authorization, cycle protection, and a global namespace.

After this lands:
- `Subject::Group(Id)` can be granted permissions on files/folders, with
  membership cascading through nested groups.
- A `Subject::User(Id)` is reached via direct grant **OR** via membership in
  any group (transitively) that holds a grant.
- One predefined immutable virtual group `Internal` represents *all internal
  users* (`is_external = false`), the way "Everyone in your org" works in
  Google Workspace.
- Groups are admin-managed (creation, naming, membership).
- Group names are RFC 5321 local-part compliant so the door to a future
  mailing-list / email-addressable feature stays open.
- Performance: recursive CTE for transitive expansion, fronted by a 30s Moka
  cache keyed by user_id. Designed so a future closure-table migration is a
  swap-in behind one function.

**Decisions accepted earlier in the conversation (encoded in this plan):**
- Max nesting depth: **8**.
- Cycle detection: **at write time** (rejects mutations).
- Cascade-delete grants when a group is deleted.
- No `Everyone` virtual group; external users are only reached via explicit
  per-grant action.
- `UseAsSubject` permission acknowledged as future work — v1 ships
  admin-only group management (anyone can target any group in a grant).
- Audit events emit via `tracing::info!(target = "audit", ...)`; a syslog
  subscriber hook is documented but its concrete wiring is a follow-up.

## Scope

### In scope (v1)

1. New tables `auth.subject_groups` + `auth.subject_group_members`.
2. Predefined `Internal` virtual group (well-known UUID, immutable).
3. CRUD REST API at `/api/groups/...` (admin-only).
4. Membership add/remove with cycle + depth checks.
5. Transitive-expansion function in `AuthorizationEngine`, Moka-cached.
6. Cascade queries in `pg_acl_engine.rs` updated to use `subject_id = ANY(...)`.
7. Share-dialog autocomplete: extend to also return groups (via the new
   authenticated `/api/groups/search` endpoint).
8. Audit logging via structured `tracing::info!(target = "audit", ...)`.
9. Minimal i18n: API-returned error message keys only.

### Out of scope (v2 / later)

- **Admin UI for group management.** v1 is API-only — `POST /api/groups`,
  member add/remove, etc. are reachable via curl / Hurl until a dedicated
  admin tab is added in a follow-up. The autocomplete extension in the
  *share dialog* (file/folder sharing UX) is the only UI change in v1.
- `Manage` and `UseAsSubject` permissions on groups themselves (delegated
  group admin requires adding `subject_group` to the `access_grants`
  resource_type CHECK and per-group authz).
- Mailing-list dispatcher (the RFC-compliant naming preserves the door).
- Concrete syslog appender wiring (env-var driven `tracing-syslog` or
  `tracing-journald` subscriber — code emits structured events today,
  operators choose a sink).
- Closure table for transitive membership (Moka cache is enough; future
  swap behind `expand_subject()`).

## Schema migration

New file: `migrations/20260612000000_subject_groups.sql`.

```sql
-- ── auth.subject_groups: root-owned authorization principals ─────────────
CREATE TABLE IF NOT EXISTS auth.subject_groups (
    id          UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    name        CITEXT       NOT NULL,
    description TEXT,
    is_virtual  BOOLEAN      NOT NULL DEFAULT FALSE,
    created_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),

    -- RFC 5321 local-part: starts alnum, then alnum/dot/dash/underscore,
    -- max 64 chars. Future-proofs `group@instance` mailing-list addressing.
    CONSTRAINT subject_groups_name_rfc5321
        CHECK (name ~ '^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$'),
    CONSTRAINT subject_groups_name_uq UNIQUE (name)
);

CREATE INDEX IF NOT EXISTS idx_subject_groups_is_virtual
    ON auth.subject_groups (is_virtual) WHERE is_virtual = TRUE;

-- ── auth.subject_group_members: edges (user→group or group→group) ────────
CREATE TABLE IF NOT EXISTS auth.subject_group_members (
    group_id        UUID NOT NULL REFERENCES auth.subject_groups(id) ON DELETE CASCADE,
    member_user_id  UUID         REFERENCES auth.users(id)           ON DELETE CASCADE,
    member_group_id UUID         REFERENCES auth.subject_groups(id)  ON DELETE CASCADE,
    added_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    added_by        UUID NOT NULL REFERENCES auth.users(id),

    -- Exactly one of the two member columns is set.
    CONSTRAINT subject_group_members_xor CHECK (
        (member_user_id IS NOT NULL)::int + (member_group_id IS NOT NULL)::int = 1
    ),
    -- A group can't contain itself directly.
    CONSTRAINT subject_group_members_no_self CHECK (
        member_group_id IS NULL OR member_group_id <> group_id
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_subject_group_members_user
    ON auth.subject_group_members (group_id, member_user_id)
    WHERE member_user_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_subject_group_members_group
    ON auth.subject_group_members (group_id, member_group_id)
    WHERE member_group_id IS NOT NULL;

-- For transitive expansion: "all groups a user belongs to directly"
CREATE INDEX IF NOT EXISTS idx_subject_group_members_by_user
    ON auth.subject_group_members (member_user_id)
    WHERE member_user_id IS NOT NULL;

-- For cycle check: "what groups does group X contain (immediate children)"
CREATE INDEX IF NOT EXISTS idx_subject_group_members_by_child_group
    ON auth.subject_group_members (member_group_id, group_id)
    WHERE member_group_id IS NOT NULL;

-- ── Seed the predefined `Internal` virtual group ─────────────────────────
-- Well-known UUID hard-coded in Rust so application code can reference it
-- without a runtime lookup: 00000000-0000-0000-0000-000000000001.
INSERT INTO auth.subject_groups (id, name, description, is_virtual)
VALUES (
    '00000000-0000-0000-0000-000000000001',
    'Internal',
    'All internal users (is_external = false). Membership is implicit; no rows in subject_group_members.',
    TRUE
)
ON CONFLICT (id) DO NOTHING;
```

**Notes on the schema:**
- `CITEXT` extension is already enabled elsewhere — confirm via
  `\dx` against a dev DB; if not, the migration must `CREATE EXTENSION IF NOT
  EXISTS citext;` first.
- The two member columns (`member_user_id`, `member_group_id`) form a tagged
  union enforced by the XOR check. Simpler than polymorphic FKs.
- The `is_virtual = TRUE` flag distinguishes the immutable `Internal` group;
  the service layer rejects member mutations on it.
- No `created_by` column — groups have no owner by design. `added_by` on
  *memberships* still records who performed the edit, for audit.

## Domain layer

New file: `src/domain/entities/subject_group.rs`.

```rust
pub struct SubjectGroup {
    pub id:          Uuid,
    pub name:        String,
    pub description: Option<String>,
    pub is_virtual:  bool,
    pub created_at:  DateTime<Utc>,
    pub updated_at:  DateTime<Utc>,
}

pub enum GroupMember {
    User(Uuid),
    Group(Uuid),
}

impl SubjectGroup {
    pub fn new(name: &str, description: Option<String>) -> Result<Self, DomainError> {
        Self::validate_name(name)?;
        // ...
    }

    /// Enforce RFC 5321 local-part shape at the domain layer too (defence in
    /// depth — the DB CHECK constraint is the authority).
    fn validate_name(name: &str) -> Result<(), DomainError> {
        static RE: Lazy<Regex> = Lazy::new(|| {
            Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$").unwrap()
        });
        if !RE.is_match(name) { return Err(DomainError::invalid("group.name.rfc5321")); }
        Ok(())
    }
}

pub const INTERNAL_GROUP_ID: Uuid = uuid!("00000000-0000-0000-0000-000000000001");
pub const MAX_GROUP_DEPTH: u8 = 8;
```

New file: `src/domain/repositories/subject_group_repository.rs` (trait).

Methods:
- `create(group: SubjectGroup) -> Result<SubjectGroup, DomainError>`
- `get_by_id(id: Uuid) -> Result<Option<SubjectGroup>, DomainError>`
- `get_by_name(name: &str) -> Result<Option<SubjectGroup>, DomainError>` (case-insensitive via CITEXT)
- `list(limit, offset, name_query: Option<&str>) -> Result<(Vec<SubjectGroup>, u64 /*total*/), DomainError>`
- `rename(id: Uuid, new_name: &str) -> Result<SubjectGroup, DomainError>`
- `delete(id: Uuid) -> Result<(), DomainError>` (cascade-deletes grants via FK from access_grants — TODO confirm; if no FK exists, also delete grants in the same transaction)
- `add_member(group_id: Uuid, member: GroupMember, added_by: Uuid) -> Result<(), DomainError>`
- `remove_member(group_id: Uuid, member: GroupMember) -> Result<(), DomainError>`
- `list_direct_members(group_id: Uuid) -> Result<Vec<GroupMember>, DomainError>`
- `list_transitive_users(group_id: Uuid) -> Result<Vec<Uuid>, DomainError>` (debug/audit)
- `groups_for_user(user_id: Uuid) -> Result<HashSet<Uuid>, DomainError>` (the hot path — recursive CTE)
- `would_introduce_cycle(parent: Uuid, candidate_child_group: Uuid) -> Result<bool, DomainError>`
- `current_depth(group_id: Uuid) -> Result<u8, DomainError>` (longest path from this group to any leaf)

## Infrastructure layer

New file:
`src/infrastructure/repositories/pg/subject_group_pg_repository.rs`.

Two queries deserve attention because the rest are straight CRUD.

### Cycle detection (write-time)

```sql
-- Adding member_group_id=$candidate to group_id=$parent introduces a cycle
-- iff $parent is reachable from $candidate by walking child-edges.
WITH RECURSIVE descendants AS (
    SELECT member_group_id AS g
    FROM auth.subject_group_members
    WHERE group_id = $candidate AND member_group_id IS NOT NULL

    UNION  -- de-dup; the union of disjoint paths is OK

    SELECT m.member_group_id
    FROM auth.subject_group_members m
    JOIN descendants d ON m.group_id = d.g
    WHERE m.member_group_id IS NOT NULL
)
SELECT 1 FROM descendants WHERE g = $parent LIMIT 1;
```

If this returns a row → reject with `DomainError::invalid("group.cycle")`.

### Transitive expansion: groups_for_user

```sql
WITH RECURSIVE user_groups AS (
    -- Base: direct memberships
    SELECT group_id FROM auth.subject_group_members
    WHERE member_user_id = $1

    UNION

    -- Recursive: groups containing those groups
    SELECT m.group_id
    FROM auth.subject_group_members m
    JOIN user_groups ug ON m.member_group_id = ug.group_id
)
SELECT group_id FROM user_groups;
```

The depth cap (`MAX_GROUP_DEPTH = 8`) is enforced at *write* time, so this
recursion is bounded by the data — Postgres has no depth limit on the CTE
itself.

### Depth check (write-time)

```sql
-- The depth of group $parent after adding $child as a member group =
-- depth-of($parent before mutation) + (1 + depth-of-subtree-rooted-at($child))
-- Simpler: compute longest path from $parent across the proposed graph and
-- reject if it would exceed 8.
WITH RECURSIVE path AS (
    SELECT group_id AS g, 1 AS depth
    FROM auth.subject_group_members WHERE group_id = $parent
    UNION
    SELECT m.group_id, p.depth + 1
    FROM auth.subject_group_members m
    JOIN path p ON m.member_group_id = p.g
)
SELECT COALESCE(MAX(depth), 0) FROM path;
```

If the post-mutation projection of this would exceed `MAX_GROUP_DEPTH = 8`,
reject with `DomainError::invalid("group.depth_exceeded")`.

In practice both checks can be combined in the same transaction, run with
`FOR UPDATE` on the parent group row to prevent concurrent racing mutations
from each squeezing under the limit individually.

## Application services

New file: `src/application/services/subject_group_service.rs`.

Methods mirror the repository trait, plus:
- Each mutator emits one structured audit event (`tracing::info!(target = "audit", ...)`).
- `add_member` runs cycle + depth checks in the same transaction as the insert.
- `delete` is guarded against removing `is_virtual = TRUE` groups.
- Service exposes one fast path: `is_user_in_group(user_id, group_id) -> bool`,
  used by the `Internal` group check (special-cased as
  `!user.is_external` — no DB hit).

Wire the service into `AppState::services` in `src/common/di.rs` alongside
the other application services.

## AuthorizationEngine extension

Modify `src/application/ports/authorization_ports.rs`:

Add a new helper on the trait (default impl can be provided in the trait,
overridden by `PgAclEngine`):

```rust
/// Returns the caller plus the IDs of every group they belong to transitively,
/// plus the predefined `INTERNAL_GROUP_ID` when the caller is not external.
/// This is the single place transitive membership is walked — all v1
/// callers, and the future closure-table swap-in, go through this function.
async fn expand_subject(&self, user_id: Uuid) -> Result<Arc<HashSet<Uuid>>, DomainError>;
```

Modify `src/infrastructure/services/pg_acl_engine.rs`:

1. Add a Moka cache field on the struct:

   ```rust
   user_groups_cache: moka::future::Cache<Uuid, Arc<HashSet<Uuid>>>,
   ```

   constructed with:

   ```rust
   Cache::builder()
       .max_capacity(50_000)
       .time_to_live(Duration::from_secs(30))
       .build();
   ```

2. Implement `expand_subject`:

   ```rust
   async fn expand_subject(&self, user_id: Uuid) -> Result<Arc<HashSet<Uuid>>, DomainError> {
       if let Some(cached) = self.user_groups_cache.get(&user_id).await {
           return Ok(cached);
       }
       let direct = self.repo.groups_for_user(user_id).await?;  // recursive CTE
       let mut set = HashSet::with_capacity(direct.len() + 2);
       set.insert(user_id);
       set.extend(direct);
       // Internal virtual group: implicit for every non-external user.
       if !self.users.is_external(user_id).await? {
           set.insert(INTERNAL_GROUP_ID);
       }
       let arc = Arc::new(set);
       self.user_groups_cache.insert(user_id, arc.clone()).await;
       Ok(arc)
   }
   ```

3. Modify the existing cascade queries (`folder_cascade_grant_exists` at
   lines 92–121 and `file_cascade_grant_exists` at lines 125–168 of
   `pg_acl_engine.rs`):

   Replace `g.subject_id = $2` with `g.subject_id = ANY($2)` and bind a
   `Vec<Uuid>` produced by `expand_subject(user_id).await?.iter().copied().collect()`.

   Subject_type must also be relaxed: today the query passes
   `subject_type = 'user'`. With groups, the helper should match against
   `subject_type IN ('user', 'group')`. (Tokens and externals are not part
   of this path; they have their own auth flows.)

The shape of the rest of the query — and the folder/file ltree cascade — is
unchanged. The closure-table migration (future) will only re-implement
`groups_for_user` against a precomputed table; callers stay the same.

## REST API

New file: `src/interfaces/api/handlers/subject_group_handler.rs`.
Wire into `src/interfaces/api/routes.rs` alongside `admin_handler`.

| Method | Route | Body / Query | Guard |
|---|---|---|---|
| POST   | `/api/groups`                            | `{ name, description? }`           | admin only |
| GET    | `/api/groups`                            | `?limit&offset&q`                  | admin only |
| GET    | `/api/groups/{id}`                       | —                                  | admin only |
| PATCH  | `/api/groups/{id}`                       | `{ name?, description? }`          | admin only |
| DELETE | `/api/groups/{id}`                       | —                                  | admin only |
| POST   | `/api/groups/{id}/members`               | `{ user_id?, group_id? }` (XOR)    | admin only |
| GET    | `/api/groups/{id}/members`               | direct members                     | admin only |
| GET    | `/api/groups/{id}/effective-members`     | transitive resolved users          | admin only |
| DELETE | `/api/groups/{id}/members/user/{uid}`    | —                                  | admin only |
| DELETE | `/api/groups/{id}/members/group/{gid}`   | —                                  | admin only |
| GET    | `/api/groups/{id}/grants`                | grants where this group is subject | admin only |
| GET    | `/api/groups/{id}/path-to-user/{uid}`    | audit: explain membership          | admin only |

Plus the share-dialog endpoint extension:

| Method | Route | Body / Query | Guard |
|---|---|---|---|
| GET    | `/api/groups/search`                     | `?q=` returns non-virtual groups whose name matches | authenticated |

This new search endpoint is **authenticated, not admin-gated** — any user can
discover groups to share with. Returns name + id only (no membership list).

Admin guard implementation: mirror `admin_handler.rs:64-100` exactly (extract
JWT, check `claims.role == "admin"`, 403 otherwise). Extract into a shared
helper `require_admin(state, headers) -> Result<(Uuid, String), AppError>`
in `interfaces/middleware/` so the new handler and `admin_handler` both use
the same code path.

## Audit logging

Convention: every mutating service-layer action emits one
`tracing::info!(target = "audit", ...)` event with structured fields.
Example:

```rust
tracing::info!(
    target: "audit",
    event = "group.member_added",
    group_id = %group_id,
    member_user_id = ?member_user_id,
    member_group_id = ?member_group_id,
    added_by = %caller_id,
);
```

Events to emit:
- `group.created` { group_id, name, created_by }
- `group.renamed` { group_id, old_name, new_name, by }
- `group.deleted` { group_id, name, by }
- `group.member_added` { group_id, member, by }
- `group.member_removed` { group_id, member, by }
- `group.cycle_rejected` { parent, candidate_child, by } (security-relevant)
- `group.depth_exceeded` { parent, by, attempted_depth }

The plan does *not* wire a syslog appender; downstream operators add a
`tracing-syslog` or `tracing-journald` subscriber via env-var-driven config.
A follow-up issue should be opened for that.

## Debug instrumentation (perf observability)

Distinct from the audit log: every authorization check emits one structured
`tracing::debug!` line with timing and cache-hit telemetry so the closure-
table-vs-cache decision (Option 2 → Option 3 in the design doc) can be made
on real data rather than speculation.

Implementation: wrap each call to `AuthorizationEngine::check` /
`AuthorizationEngine::expand_subject` in a tracing span and increment
per-call counters. Suggested shape:

```rust
impl PgAclEngine {
    async fn check(&self, subject: Subject, perm: Permission, resource: Resource)
        -> Result<bool, DomainError>
    {
        let start = std::time::Instant::now();
        let counters = QueryCounters::default();

        let result = self.check_inner(subject, perm, resource, &counters).await;

        tracing::debug!(
            event = "authz.check",
            subject = ?subject,
            permission = ?perm,
            resource = ?resource,
            allowed = result.as_ref().ok().copied().unwrap_or(false),
            duration_us = start.elapsed().as_micros() as u64,
            cache_hit = counters.cache_hit.load(Ordering::Relaxed),
            sql_queries = counters.sql_queries.load(Ordering::Relaxed),
            expanded_groups = counters.expanded_group_count.load(Ordering::Relaxed),
        );

        result
    }
}
```

Where `QueryCounters` is a tiny struct of `AtomicU32`s passed through the
call chain, incremented at each `sqlx::query*` call site inside the authz
path. `cache_hit` is set by `expand_subject` based on whether the Moka
`Cache::get` returned `Some`.

**What this gives you:**

| Field | Use |
|---|---|
| `duration_us` | latency histogram per check; alert on p99 regression |
| `cache_hit` | hit-rate metric → decide when to extend TTL or switch to closure table |
| `sql_queries` | 0 on cache hit; 1 on cache miss + grant lookup; 2 if cache miss + transitive expansion + grant lookup — confirms the query plan in production |
| `expanded_groups` | size of the user's transitive group set; if this stays small in practice, the recursive CTE is more than enough |

**Cost:** sub-microsecond per check (atomic increments + a single
`tracing::debug!` emission, which becomes a no-op when the subscriber is at
INFO or higher). No runtime cost in production unless debug logging is
explicitly enabled.

**Recommended deployment hook:** an env var `OXICLOUD_AUTHZ_DEBUG=true` that
flips the subscriber filter to allow `target="oxicloud::authz" level=debug`
events through. Operators turn it on temporarily when investigating
performance issues; default is INFO and emits nothing from this path.

## Share dialog extension

Modify `static/js/components/shareModal.js` around line 310.

Currently it calls `addressBook.searchContacts(q, [SYSTEM_BOOK_ID])`. Add a
parallel call to `fetch('/api/groups/search?q=' + encodeURIComponent(q))`.
Merge the two result lists, tag each item by source (`user` vs `group`
vs `contact`), and render with the appropriate icon (user avatar /
`fa-layer-group` / contact card).

On selection, dispatch to the existing grant-creation flow with the
correct `subject_type`:
- `user` → `subject_type = 'user'`, `subject_id = user.id`
- `group` → `subject_type = 'group'`, `subject_id = group.id`
- `contact` → resolved through the existing address-book mapper to the
  matching user_id (no change from today)

## i18n

Add to `static/locales/en.json` (errors surfaced by the API + share-dialog):

```json
"errors": {
  "group_name_invalid": "Group name must match the email-prefix format (letters, digits, dot, dash, underscore; 1–64 chars).",
  "group_cycle": "This member would create a circular group reference.",
  "group_depth_exceeded": "This nesting depth exceeds the maximum allowed (8).",
  "group_virtual_immutable": "The 'Internal' group is system-managed and cannot be modified.",
  "group_not_found": "Group not found."
}
```

These keys are referenced by `ApiError` payloads from the new handler and
by the share-dialog UI when a target group is invalid. Sync the 15 locale
files using the Python script pattern from the earlier i18n turn.

The full set of admin-table labels (`admin.tab_groups`, `admin.col_*`, etc.)
is **deferred to the v2 admin UI work** along with the rest of the admin
surface for groups.

## Tests

### Unit tests

Module: `src/infrastructure/repositories/pg/subject_group_pg_repository.rs#tests`

1. `test_create_group_validates_name_rfc5321` — names with spaces / emojis
   rejected; valid names accepted.
2. `test_group_name_unique_case_insensitive` — "Engineering" and
   "engineering" collide (CITEXT).
3. `test_cycle_check_rejects_direct_loop` — adding A to A rejected by the
   `no_self` CHECK or by the cycle CTE.
4. `test_cycle_check_rejects_two_step_loop` — A∋B, B∋C, attempting C∋A
   rejected.
5. `test_cycle_check_rejects_eight_step_loop` — same with longer chain.
6. `test_depth_cap_at_8` — adding a 9th level rejected.
7. `test_transitive_expansion_includes_indirect_groups` — A∋B, B∋C, U∈A
   returns {A, B, C} (plus U and Internal).
8. `test_internal_group_implicit_for_internal_users` — non-external user's
   expansion contains `INTERNAL_GROUP_ID`; external user's doesn't.
9. `test_virtual_group_cannot_be_deleted` — service rejects delete on the
   Internal group.
10. `test_member_can_be_user_or_group_but_not_both` — XOR check.

### Integration tests

11. `test_authz_cascades_through_group` — Alice in group G; G has read grant
    on file F; AuthorizationEngine::check returns Allow.
12. `test_authz_cascades_through_nested_group` — Alice in B, B in A, A has
    grant. Expect Allow.
13. `test_grant_revoked_when_group_deleted` — delete G; previous
    G-mediated grants no longer apply (FK CASCADE).
14. `test_user_removed_from_group_loses_access_after_cache_ttl` — remove
    Alice from G; within 30s old answer may persist; after TTL, denied.
15. `test_internal_group_grant_visible_to_all_internal` — grant `read` on
    file F to `INTERNAL_GROUP_ID`; every internal user can read F; no
    external user can.

### API tests (Hurl)

16. `tests/api/groups_admin_only.hurl` — non-admin POST /api/groups → 403.
17. `tests/api/groups_crud_happy_path.hurl` — create, list, get, rename,
    delete.
18. `tests/api/groups_member_lifecycle.hurl` — add user, add nested
    group, remove user, remove nested group.
19. `tests/api/groups_invalid_name.hurl` — name with space → 400.

## Verification

Pre-commit:

```bash
cargo fmt --all
cargo clippy --all-features --all-targets -- -D warnings
cargo test --workspace
biome check --fix static/js/
stylelint static/css/
tsc -p jsconfig.json --noEmit
```

End-to-end smoke test (manual; v1 is API-driven):

1. `docker compose up -d postgres`.
2. `cargo run`.
3. Obtain an admin JWT (log in via the existing login flow, copy the access
   token).
4. Create a group:
   ```
   curl -X POST /api/groups -H 'Authorization: Bearer …' \
     -d '{"name":"engineering"}'
   ```
5. Confirm name validation rejects `Engineering Team` (returns 400 with
   `error_code: group_name_invalid`).
6. Add yourself as a member:
   ```
   curl -X POST /api/groups/<gid>/members -d '{"user_id":"<you>"}'
   ```
7. Create a second group `qa`, then add `engineering` as a nested member of
   `qa`. Confirm with `GET /api/groups/<qa_id>/members`.
8. Attempt to add `qa` as a member of `engineering` — expect 400 with
   `error_code: group_cycle`.
9. In the browser, open a file → share dialog → type `eng`. The
   `engineering` group should appear with the layer-group icon. Pick it
   and grant `read`.
10. Log in as a user who is a member of `engineering` (directly or via
    `qa` cascading) — confirm the file is accessible.
11. `DELETE /api/groups/<gid>/members/user/<uid>`. After 30 seconds (cache
    TTL), confirm access is denied.
12. Grant `read` on a file to the `Internal` virtual group (`subject_id`
    = `00000000-0000-0000-0000-000000000001`). Confirm every internal user
    has access. Confirm an external user (if available) does not.
13. `journalctl -t oxicloud | grep audit` (or equivalent log inspection) —
    confirm one structured log line per group mutation, with the
    `target="audit"` and `event="group.*"` fields.

## Critical files to be modified

**New files:**
- `migrations/20260612000000_subject_groups.sql`
- `src/domain/entities/subject_group.rs`
- `src/domain/repositories/subject_group_repository.rs`
- `src/infrastructure/repositories/pg/subject_group_pg_repository.rs`
- `src/application/services/subject_group_service.rs`
- `src/interfaces/api/handlers/subject_group_handler.rs`
- `tests/api/groups_*.hurl`

**Modified files:**
- `src/application/ports/authorization_ports.rs` — add `expand_subject`.
- `src/infrastructure/services/pg_acl_engine.rs` — add Moka cache field,
  implement `expand_subject`, modify `folder_cascade_grant_exists` (lines
  92–121) and `file_cascade_grant_exists` (lines 125–168) to use
  `subject_id = ANY($caller_plus_groups)`.
- `src/common/di.rs` — wire `SubjectGroupService` into `AppState`, pass
  user-repo into `PgAclEngine` constructor.
- `src/interfaces/api/routes.rs` — register the new handler.
- `src/interfaces/middleware/` — extract `require_admin` shared helper
  from `admin_handler.rs:64-100`.
- `static/js/components/shareModal.js` — parallel `/api/groups/search`
  call around line 310 to surface groups in the recipient autocomplete.
- `static/locales/en.json` + 15 locale files — new `errors.group_*` keys
  for API error rendering.

(No changes to `static/admin.html` or `static/js/views/admin/admin.js` in
v1 — admin UI is v2 work.)

## Reused utilities

- `AppState.authorization` (Arc<PgAclEngine>) — existing DI wiring.
- `admin_handler::admin_guard` pattern (`admin_handler.rs:64-100`) — extract
  shared.
- `moka` 0.12.15 (`Cargo.toml:38`) — already present.
- `tracing::info!` — existing observability pipeline; just add the
  `target: "audit"` convention.
- `ResourceListComponent` and `userVignette` — already used by other
  admin tables; reuse for the Groups admin table.
- Recursive CTE pattern — new to OxiCloud but standard Postgres.
- `auth.users.role = 'admin'` ENUM check — admin guard.
