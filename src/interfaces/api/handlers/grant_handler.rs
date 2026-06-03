//! REST handlers for the ReBAC grant management endpoints.
//!
//! All endpoints under `/api/grants`. The authenticated caller is taken from
//! the `AuthUser` extractor. Authorization for sharing operations is enforced
//! via `authz.require(caller, Share, resource)` — handlers never embed their
//! own checks (see CLAUDE.md § Authorization).

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use futures::future::join_all;
use serde::Deserialize;
use std::sync::Arc;
use tracing::{error, info, warn};
use utoipa::IntoParams;
use uuid::Uuid;

use crate::application::dtos::cursor::PageCursor;
use crate::application::dtos::grant_dto::{
    CreateGrantDto, GrantDto, MySharesDto, OutgoingResourceGrantDto, OutgoingResourceItemDto,
    PermissionDto, ResourceContentDto, ResourceDto, ResourceTypeDto, SharedWithMeDto,
    SharedWithMeItemDto, SharedWithMeQuery, SubjectDto, SubjectInputDto, UpdateRoleDto,
    role_from_permissions,
};
use crate::application::ports::authorization_ports::AuthorizationEngine;
use crate::application::ports::file_ports::FileRetrievalUseCase;
use crate::application::ports::folder_ports::FolderUseCase;
use crate::common::di::AppState;
#[allow(unused_imports)]
use crate::common::errors::DomainError;
use crate::domain::errors::ErrorKind;
use crate::domain::services::authorization::{
    GrantCursor, IncomingGrantSummary, OutgoingResourceSummary, Permission, Resource, ResourceKind,
    Subject,
};
use crate::interfaces::errors::AppError;
use crate::interfaces::middleware::auth::AuthUser;

type AppStateRef = Arc<AppState>;

// ════════════════════════════════════════════════════════════════════════════
// POST /api/grants
// ════════════════════════════════════════════════════════════════════════════

#[utoipa::path(
    post,
    path = "/api/grants",
    request_body = CreateGrantDto,
    responses(
        (status = 201, description = "Grant(s) created", body = Vec<GrantDto>),
        (status = 400, description = "Invalid input (both/neither of permissions+role provided)"),
        (status = 404, description = "Resource not found OR caller lacks Share permission"),
    ),
    security(("bearerAuth" = [])),
    tag = "grants"
)]
pub async fn create_grant(
    State(state): State<AppStateRef>,
    auth_user: AuthUser,
    Json(dto): Json<CreateGrantDto>,
) -> impl IntoResponse {
    let authz = &state.authorization;
    let caller_id = auth_user.id;

    // Validate: exactly one of permissions/role
    let permissions: Vec<Permission> = match (dto.permissions, dto.role) {
        (Some(perms), None) if !perms.is_empty() => perms.into_iter().map(Into::into).collect(),
        (None, Some(role)) => role.expand().to_vec(),
        (Some(_), Some(_)) => {
            return AppError::new(
                StatusCode::BAD_REQUEST,
                "Provide either 'permissions' or 'role', not both",
                "InvalidInput",
            )
            .into_response();
        }
        _ => {
            return AppError::new(
                StatusCode::BAD_REQUEST,
                "Either 'permissions' (non-empty) or 'role' is required",
                "InvalidInput",
            )
            .into_response();
        }
    };

    let resource: Resource = dto.resource.into();
    let expires_at = dto.expires_at;

    // Caller must have Share on the resource (owners pass via short-circuit).
    if let Err(e) = authz
        .require(Subject::User(caller_id), Permission::Share, resource)
        .await
    {
        return AppError::from(e).into_response();
    }

    // Resolve the subject. For the email variant this lazily provisions
    // an external user (or reuses an existing match) and remembers the
    // resolved User so the invitation email can be sent after the grant
    // rows land.
    let (subject, invite_recipient) = match dto.subject {
        SubjectInputDto::User { id } => (Subject::User(id), None),
        SubjectInputDto::Group { id } => (Subject::Group(id), None),
        SubjectInputDto::Token { id } => (Subject::Token(id), None),
        SubjectInputDto::Email { email } => {
            let Some(invite_svc) = state.magic_link_invite_service.as_ref() else {
                return AppError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Magic-link invitations are not configured on this server \
                     (set OXICLOUD_SMTP_HOST in .env to enable)",
                    "ServiceUnavailable",
                )
                .into_response();
            };
            // PR 12 — per-sharer ceiling: 50 email-invitations / hour
            // per caller. Hitting the cap returns 429 because the
            // caller is authenticated and rate-limit visibility leaks
            // nothing they don't already know about their own
            // behaviour.
            if state
                .email_invite_rate_limiter
                .check_and_increment(&caller_id.to_string())
                .is_err()
            {
                tracing::warn!(
                    target: "audit",
                    event = "grants.email_invite",
                    reason = "rate_limited",
                    caller_id = %caller_id,
                    "Per-sharer email-invite rate limit exceeded"
                );
                return crate::interfaces::middleware::rate_limit::too_many_requests(
                    state.email_invite_rate_limiter.retry_after(),
                );
            }
            // PR C: pass the inviter id so resolve_or_create_recipient
            // can inherit their preferred_locale onto a freshly-
            // provisioned external user (best-effort; lookup failure
            // just leaves the new row's locale NULL, no hard error).
            match invite_svc
                .resolve_or_create_recipient(&email, Some(caller_id))
                .await
            {
                Ok(user) => (Subject::User(user.id()), Some(user)),
                Err(e) => return AppError::from(e).into_response(),
            }
        }
    };

    let mut results: Vec<GrantDto> = Vec::with_capacity(permissions.len());
    for perm in permissions {
        match authz
            .grant(caller_id, subject, perm, resource, expires_at)
            .await
        {
            Ok(grant) => results.push(grant.into()),
            Err(err) => {
                error!("grant insert failed for {perm:?}: {err}");
                return AppError::from(err).into_response();
            }
        }
    }
    info!(
        "Created {} grant(s) for subject={:?} on resource={:?} by user {}",
        results.len(),
        subject,
        resource,
        caller_id
    );

    // Fire the invitation email AFTER the grant rows are in place so a
    // failed SMTP send can't leave the recipient with mail-but-no-access.
    // The service swallows SMTP errors (logs only) — the API response
    // stays 201 Created either way, matching the plan's "201 always
    // when grants land; mail is best-effort" contract.
    if let Some(recipient) = invite_recipient
        && let Some(invite_svc) = state.magic_link_invite_service.as_ref()
    {
        let inviter_name = auth_user.username.clone();
        if let Err(e) = invite_svc
            .issue_invitation(&recipient, &inviter_name, resource)
            .await
        {
            warn!(
                "invitation issuance failed for {} (grants already created): {}",
                recipient.email(),
                e
            );
        }
    }

    (StatusCode::CREATED, Json(results)).into_response()
}

// ════════════════════════════════════════════════════════════════════════════
// DELETE /api/grants/{id}
// ════════════════════════════════════════════════════════════════════════════

#[utoipa::path(
    delete,
    path = "/api/grants/{id}",
    params(("id" = String, Path, description = "Grant UUID")),
    responses(
        (status = 204, description = "Grant revoked (or did not exist)"),
        (status = 404, description = "Caller lacks Share permission on the underlying resource"),
    ),
    security(("bearerAuth" = [])),
    tag = "grants"
)]
pub async fn revoke_grant(
    State(state): State<AppStateRef>,
    auth_user: AuthUser,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let authz = &state.authorization;
    let caller_id = auth_user.id;
    let grant_id = match Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => return AppError::not_found(format!("Grant {id} not found")).into_response(),
    };

    // Look up the grant to find the underlying resource (and granter).
    let on_resource = match authz.find_grant_by_id(grant_id).await {
        Ok(Some((res, granter))) => (res, granter),
        Ok(None) => return StatusCode::NO_CONTENT.into_response(), // idempotent
        Err(e) => return AppError::from(e).into_response(),
    };

    // Caller is authorized if they are the granter OR have Share on the resource.
    if on_resource.1 != caller_id
        && let Err(e) = authz
            .require(Subject::User(caller_id), Permission::Share, on_resource.0)
            .await
    {
        return AppError::from(e).into_response();
    }

    if let Err(e) = authz.revoke(grant_id).await {
        return AppError::from(e).into_response();
    }
    info!("Revoked grant {grant_id} (caller {caller_id})");
    StatusCode::NO_CONTENT.into_response()
}

// ════════════════════════════════════════════════════════════════════════════
// PUT /api/grants/role
// ════════════════════════════════════════════════════════════════════════════

#[utoipa::path(
    put,
    path = "/api/grants/role",
    request_body = UpdateRoleDto,
    responses(
        (status = 200, description = "Role applied; returns the new full grant set", body = Vec<GrantDto>),
        (status = 404, description = "Resource not found or caller lacks Share"),
    ),
    security(("bearerAuth" = [])),
    tag = "grants"
)]
pub async fn set_role(
    State(state): State<AppStateRef>,
    auth_user: AuthUser,
    Json(dto): Json<UpdateRoleDto>,
) -> impl IntoResponse {
    let authz = &state.authorization;
    let caller_id = auth_user.id;
    let subject: Subject = dto.subject.into();
    let resource: Resource = dto.resource.into();
    let expires_at = dto.expires_at;
    let target_perms: std::collections::HashSet<Permission> =
        dto.role.expand().iter().copied().collect();

    // Caller must have Share on the resource.
    if let Err(e) = authz
        .require(Subject::User(caller_id), Permission::Share, resource)
        .await
    {
        return AppError::from(e).into_response();
    }

    // Fetch current grants on the resource for this subject.
    let current = match authz.list_grants_on_resource(resource).await {
        Ok(g) => g,
        Err(e) => return AppError::from(e).into_response(),
    };
    let current_perms: std::collections::HashSet<Permission> = current
        .iter()
        .filter(|g| g.subject == subject)
        .map(|g| g.permission)
        .collect();

    // Diff and apply.
    let to_add: Vec<Permission> = target_perms.difference(&current_perms).copied().collect();
    let to_remove: Vec<Permission> = current_perms.difference(&target_perms).copied().collect();

    for perm in &to_remove {
        if let Some(g) = current
            .iter()
            .find(|g| g.subject == subject && g.permission == *perm)
            && let Err(e) = authz.revoke(g.id).await
        {
            return AppError::from(e).into_response();
        }
    }
    for perm in &to_add {
        if let Err(e) = authz
            .grant(caller_id, subject, *perm, resource, expires_at)
            .await
        {
            return AppError::from(e).into_response();
        }
    }

    // Sync expiry on all remaining grants for this (subject, resource) pair —
    // includes newly added ones and any that were already present (retained).
    // Callers that omit expires_at will clear any existing expiry; this is
    // intentional: it keeps all permission rows for the pair consistent.
    if let Err(e) = authz
        .set_expiry_on_resource(subject, resource, expires_at)
        .await
    {
        return AppError::from(e).into_response();
    }

    // Return the new full set.
    let after = match authz.list_grants_on_resource(resource).await {
        Ok(g) => g,
        Err(e) => return AppError::from(e).into_response(),
    };
    let mine: Vec<GrantDto> = after
        .into_iter()
        .filter(|g| g.subject == subject)
        .map(Into::into)
        .collect();

    info!(
        "Role applied: caller={} subject={:?} resource={:?} added={:?} removed={:?}",
        caller_id, subject, resource, to_add, to_remove
    );
    (StatusCode::OK, Json(mine)).into_response()
}

// ════════════════════════════════════════════════════════════════════════════
// GET /api/grants/incoming
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize, IntoParams)]
pub struct IncomingQuery {
    #[serde(default)]
    pub permission: Option<PermissionDto>,
}

#[utoipa::path(
    get,
    path = "/api/grants/incoming",
    params(IncomingQuery),
    responses(
        (status = 200, description = "Direct grants targeting the caller", body = Vec<GrantDto>),
    ),
    security(("bearerAuth" = [])),
    tag = "grants"
)]
pub async fn list_incoming(
    State(state): State<AppStateRef>,
    auth_user: AuthUser,
    Query(q): Query<IncomingQuery>,
) -> impl IntoResponse {
    let caller_id = auth_user.id;
    match state
        .authorization
        .list_incoming_grants(Subject::User(caller_id), q.permission.map(Into::into))
        .await
    {
        Ok(grants) => {
            let dtos: Vec<GrantDto> = grants.into_iter().map(Into::into).collect();
            (StatusCode::OK, Json(dtos)).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// GET /api/grants/incoming/resources
// ════════════════════════════════════════════════════════════════════════════

#[utoipa::path(
    get,
    path = "/api/grants/incoming/resources",
    params(SharedWithMeQuery),
    responses(
        (status = 200,
         description = "Cursor-paginated resources shared with the caller. \
                        Each item carries the full file or folder details plus \
                        aggregated permissions. `next_cursor` is absent on the \
                        last page.",
         body = SharedWithMeDto),
    ),
    security(("bearerAuth" = [])),
    tag = "grants"
)]
pub async fn list_shared_with_me(
    State(state): State<AppStateRef>,
    auth_user: AuthUser,
    Query(q): Query<SharedWithMeQuery>,
) -> impl IntoResponse {
    let caller_id = auth_user.id;
    let subject = Subject::User(caller_id);

    // Parse resource_types filter (unknown values silently ignored).
    let kinds: Vec<ResourceKind> = q
        .resource_types
        .as_deref()
        .map(|s| {
            s.split(',')
                .filter_map(|t| ResourceKind::parse(t.trim()))
                .collect()
        })
        .unwrap_or_default();

    // Clamp limit to 1–200.
    let limit = q.limit_clamped() as u32;

    // Validate sort_by (defaults to "granted_at").
    let sort_by = q.sort_by.as_deref().unwrap_or("granted_at");
    if !matches!(sort_by, "granted_at" | "granted_by" | "name" | "type") {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid sort_by; valid values: granted_at, granted_by, name, type"})),
        )
            .into_response();
    }

    let reverse = q.reverse;

    // Decode cursor — discard it when the sort dimension or direction changed
    // to avoid keyset confusion across sort modes.
    let cursor = q
        .decode_cursor::<GrantCursor>()
        .filter(|c| c.sort_by == sort_by && c.reverse == reverse);

    // Fetch paged summaries from the ACL engine.
    let (summaries, next_cursor) = match state
        .authorization
        .list_incoming_resources_paged(subject, &kinds, limit, cursor, sort_by, reverse)
        .await
    {
        Ok(r) => r,
        Err(e) => return AppError::from(e).into_response(),
    };

    // Split summaries by resource kind for parallel resolution.
    let file_summaries: Vec<&IncomingGrantSummary> = summaries
        .iter()
        .filter(|s| matches!(s.resource_type, ResourceKind::File))
        .collect();
    let folder_summaries: Vec<&IncomingGrantSummary> = summaries
        .iter()
        .filter(|s| matches!(s.resource_type, ResourceKind::Folder))
        .collect();

    let file_service = &state.applications.file_retrieval_service;
    let folder_service = &state.applications.folder_service_concrete;

    // Pre-compute ID strings to avoid temporaries inside async closures.
    let file_ids: Vec<String> = file_summaries
        .iter()
        .map(|s| s.resource_id.to_string())
        .collect();
    let folder_ids: Vec<String> = folder_summaries
        .iter()
        .map(|s| s.resource_id.to_string())
        .collect();

    // Resolve resource details concurrently (files and folders in parallel).
    let (file_results, folder_results) = tokio::join!(
        join_all(file_ids.iter().map(|id| file_service.get_file(id))),
        join_all(folder_ids.iter().map(|id| folder_service.get_folder(id)))
    );

    // Build the unified item list in original grant order (newest first).
    // We iterate summaries in order and pick the resolved result from the
    // appropriate typed bucket.
    let mut file_idx = 0usize;
    let mut folder_idx = 0usize;

    let mut items: Vec<SharedWithMeItemDto> = Vec::with_capacity(summaries.len());

    for summary in &summaries {
        match summary.resource_type {
            ResourceKind::File => {
                let result = &file_results[file_idx];
                file_idx += 1;
                match result {
                    Ok(file_dto) => {
                        items.push(SharedWithMeItemDto {
                            resource_type: ResourceTypeDto::File,
                            permissions: summary.permissions.iter().map(|p| (*p).into()).collect(),
                            granted_at: summary.granted_at,
                            granted_by: summary.granted_by,
                            resource: ResourceContentDto::File(
                                file_dto.clone().without_hierarchy_info(),
                            ),
                        });
                    }
                    Err(e) if e.kind == ErrorKind::NotFound => {
                        // Stale grant (file deleted, trigger not yet fired) — skip silently.
                        warn!(
                            "Skipping stale file grant for resource_id={}: not found",
                            summary.resource_id
                        );
                    }
                    Err(e) => {
                        return AppError::internal_error(format!(
                            "Failed to fetch file {}: {e}",
                            summary.resource_id
                        ))
                        .into_response();
                    }
                }
            }
            ResourceKind::Folder => {
                let result = &folder_results[folder_idx];
                folder_idx += 1;
                match result {
                    Ok(folder_dto) => {
                        items.push(SharedWithMeItemDto {
                            resource_type: ResourceTypeDto::Folder,
                            permissions: summary.permissions.iter().map(|p| (*p).into()).collect(),
                            granted_at: summary.granted_at,
                            granted_by: summary.granted_by,
                            resource: ResourceContentDto::Folder(
                                folder_dto.clone().without_hierarchy_info(),
                            ),
                        });
                    }
                    Err(e) if e.kind == ErrorKind::NotFound => {
                        warn!(
                            "Skipping stale folder grant for resource_id={}: not found",
                            summary.resource_id
                        );
                    }
                    Err(e) => {
                        return AppError::internal_error(format!(
                            "Failed to fetch folder {}: {e}",
                            summary.resource_id
                        ))
                        .into_response();
                    }
                }
            }
        }
    }

    (
        StatusCode::OK,
        Json(SharedWithMeDto::with_cursor(
            items,
            next_cursor.map(|c| c.encode()),
        )),
    )
        .into_response()
}

// ════════════════════════════════════════════════════════════════════════════
// GET /api/grants/outgoing
// ════════════════════════════════════════════════════════════════════════════

#[utoipa::path(
    get,
    path = "/api/grants/outgoing",
    responses(
        (status = 200, description = "Grants the caller has created", body = Vec<GrantDto>),
    ),
    security(("bearerAuth" = [])),
    tag = "grants"
)]
pub async fn list_outgoing(
    State(state): State<AppStateRef>,
    auth_user: AuthUser,
) -> impl IntoResponse {
    let caller_id = auth_user.id;
    match state.authorization.list_outgoing_grants(caller_id).await {
        Ok(grants) => {
            let dtos: Vec<GrantDto> = grants.into_iter().map(Into::into).collect();
            (StatusCode::OK, Json(dtos)).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// GET /api/grants?resource_type=...&resource_id=...
// (list grants on a specific resource — requires Share on it)
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize, IntoParams)]
pub struct OnResourceQuery {
    pub resource_type: ResourceTypeDto,
    pub resource_id: Uuid,
}

#[utoipa::path(
    get,
    path = "/api/grants",
    params(OnResourceQuery),
    responses(
        (status = 200, description = "Grants on the specified resource", body = Vec<GrantDto>),
        (status = 404, description = "Resource not found or caller lacks Share"),
    ),
    security(("bearerAuth" = [])),
    tag = "grants"
)]
pub async fn list_on_resource(
    State(state): State<AppStateRef>,
    auth_user: AuthUser,
    Query(q): Query<OnResourceQuery>,
) -> impl IntoResponse {
    let authz = &state.authorization;
    let caller_id = auth_user.id;
    let resource: Resource = ResourceDto {
        kind: q.resource_type,
        id: q.resource_id,
    }
    .into();

    if let Err(e) = authz
        .require(Subject::User(caller_id), Permission::Share, resource)
        .await
    {
        return AppError::from(e).into_response();
    }

    match authz.list_grants_on_resource(resource).await {
        Ok(grants) => {
            let dtos: Vec<GrantDto> = grants.into_iter().map(Into::into).collect();
            (StatusCode::OK, Json(dtos)).into_response()
        }
        Err(e) => AppError::from(e).into_response(),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// GET /api/grants/outgoing/resources
// ════════════════════════════════════════════════════════════════════════════

#[utoipa::path(
    get,
    path = "/api/grants/outgoing/resources",
    params(SharedWithMeQuery),
    responses(
        (status = 200,
         description = "Cursor-paginated resources the caller has shared with others. \
                        Each item carries the full resource details plus all subjects \
                        (users and tokens) the resource was shared with. \
                        `next_cursor` is absent on the last page.",
         body = MySharesDto),
    ),
    security(("bearerAuth" = [])),
    tag = "grants"
)]
pub async fn list_my_shares(
    State(state): State<AppStateRef>,
    auth_user: AuthUser,
    Query(q): Query<SharedWithMeQuery>,
) -> impl IntoResponse {
    let caller_id = auth_user.id;

    let limit = q.limit_clamped() as u32;

    let sort_by = q.sort_by.as_deref().unwrap_or("first_shared_at");
    if !matches!(
        sort_by,
        "first_shared_at" | "name" | "type" | "subject" | "role"
    ) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid sort_by; valid values: first_shared_at, name, type, subject, role"})),
        )
            .into_response();
    }

    let reverse = q.reverse;

    let cursor = q
        .decode_cursor::<GrantCursor>()
        .filter(|c| c.sort_by == sort_by && c.reverse == reverse);

    let (summaries, next_cursor) = match state
        .authorization
        .list_outgoing_resources_paged(caller_id, limit, cursor, sort_by, reverse)
        .await
    {
        Ok(r) => r,
        Err(e) => return AppError::from(e).into_response(),
    };

    let file_service = &state.applications.file_retrieval_service;
    let folder_service = &state.applications.folder_service_concrete;

    // Split summaries by resource kind for parallel resolution.
    let file_summaries: Vec<&OutgoingResourceSummary> = summaries
        .iter()
        .filter(|s| matches!(s.resource_type, ResourceKind::File))
        .collect();
    let folder_summaries: Vec<&OutgoingResourceSummary> = summaries
        .iter()
        .filter(|s| matches!(s.resource_type, ResourceKind::Folder))
        .collect();

    let file_ids: Vec<String> = file_summaries
        .iter()
        .map(|s| s.resource_id.to_string())
        .collect();
    let folder_ids: Vec<String> = folder_summaries
        .iter()
        .map(|s| s.resource_id.to_string())
        .collect();

    let (file_results, folder_results) = tokio::join!(
        join_all(file_ids.iter().map(|id| file_service.get_file(id))),
        join_all(folder_ids.iter().map(|id| folder_service.get_folder(id)))
    );

    let mut file_idx = 0usize;
    let mut folder_idx = 0usize;
    let mut items: Vec<OutgoingResourceItemDto> = Vec::with_capacity(summaries.len());

    for summary in &summaries {
        let grants: Vec<OutgoingResourceGrantDto> = summary
            .grants
            .iter()
            .map(|g| OutgoingResourceGrantDto {
                grant_id: g.grant_id,
                subject_type: g.subject_type.clone(),
                subject_id: g.subject_id,
                subject_display: g.subject_display.clone(),
                role: role_from_permissions(&g.permissions).to_owned(),
                granted_at: g.granted_at,
                expires_at: g.expires_at,
                has_password: g.has_password,
            })
            .collect();

        match summary.resource_type {
            ResourceKind::File => {
                let result = &file_results[file_idx];
                file_idx += 1;
                match result {
                    Ok(file_dto) => {
                        // Caller is the granter — they had share-access to the
                        // resource, so the containing hierarchy is already known
                        // to them. Keep `path` (unlike list_shared_with_me).
                        items.push(OutgoingResourceItemDto {
                            resource_type: ResourceTypeDto::File,
                            first_shared_at: summary.first_shared_at,
                            resource: ResourceContentDto::File(file_dto.clone()),
                            grants,
                        });
                    }
                    Err(e) if e.kind == ErrorKind::NotFound => {
                        warn!(
                            "Skipping stale outgoing file grant for resource_id={}: not found",
                            summary.resource_id
                        );
                    }
                    Err(e) => {
                        return AppError::internal_error(format!(
                            "Failed to fetch file {}: {e}",
                            summary.resource_id
                        ))
                        .into_response();
                    }
                }
            }
            ResourceKind::Folder => {
                let result = &folder_results[folder_idx];
                folder_idx += 1;
                match result {
                    Ok(folder_dto) => {
                        items.push(OutgoingResourceItemDto {
                            resource_type: ResourceTypeDto::Folder,
                            first_shared_at: summary.first_shared_at,
                            resource: ResourceContentDto::Folder(folder_dto.clone()),
                            grants,
                        });
                    }
                    Err(e) if e.kind == ErrorKind::NotFound => {
                        warn!(
                            "Skipping stale outgoing folder grant for resource_id={}: not found",
                            summary.resource_id
                        );
                    }
                    Err(e) => {
                        return AppError::internal_error(format!(
                            "Failed to fetch folder {}: {e}",
                            summary.resource_id
                        ))
                        .into_response();
                    }
                }
            }
        }
    }

    (
        StatusCode::OK,
        Json(MySharesDto::with_cursor(
            items,
            next_cursor.map(|c| c.encode()),
        )),
    )
        .into_response()
}

// Silence unused-import warnings for SubjectDto when only certain endpoints
// touch it directly.
#[allow(dead_code)]
fn _ensure_subject_dto_compiles(_: SubjectDto) {}
