//! Magic-link redemption endpoint.
//!
//! Single public route: `GET /magic/v1/{token}`. Validating the token is
//! the entire authentication — the URL is the credential.
//!
//! Successful redemption:
//!   1. Atomically marks the token used (single-use, race-free).
//!   2. Issues access + refresh JWT for the token's owning user.
//!   3. Sets the standard `oxicloud_access` / `oxicloud_refresh` /
//!      `oxicloud_csrf` cookies (same as `POST /api/auth/login`).
//!   4. 302-redirects to a frontend hash-route based on the token's
//!      resource target:
//!        - Folder       → `/#/files/folder/{id}`
//!        - File or NULL → `/#/sharedwithme`
//!
//! Files don't have a deep-link route today; v1 lands file invitations
//! on Shared With Me where the file shows up.
//!
//! Failure cases (all return 4xx without setting cookies):
//!   - Token not found / expired / already used → 410 Gone.
//!   - Magic-link feature disabled (no SMTP / repo) → 503.
//!   - Owning user deactivated → 410 Gone.

use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::CONTENT_TYPE, header::LOCATION},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;

use crate::application::services::auth_application_service::{
    MagicLinkRedeemResult, MagicLinkRedemption,
};
use crate::common::di::AppState;
use crate::common::errors::ErrorKind;
use crate::domain::entities::magic_link_token::MagicLinkResourceKind;
use crate::interfaces::api::cookie_auth;

/// Build the `/magic/v1/{token}` router. Mounted at the top of the
/// application tree in `main.rs` — no auth middleware, no CSRF (the
/// token is the credential, the route is GET-only).
pub fn magic_link_routes() -> Router<Arc<AppState>> {
    Router::new().route("/magic/v1/{token}", get(redeem_magic_link))
}

#[derive(Debug, Deserialize)]
struct RedeemQuery {
    /// PR 22: `?confirm=1` means the user clicked the cross-browser
    /// confirmation prompt's Continue button. The service skips the
    /// challenge-cookie check on this re-entry.
    #[serde(default)]
    confirm: Option<String>,
}

#[utoipa::path(
    get,
    path = "/magic/v1/{token}",
    params(("token" = String, Path, description = "Opaque magic-link token")),
    responses(
        (status = 200, description = "Cross-browser confirmation prompt (HTML page)"),
        (status = 302, description = "Redemption succeeded — redirects to the resource or to /#/sharedwithme"),
        (status = 410, description = "Token is unknown, expired, or already used"),
        (status = 503, description = "Magic-link feature is not configured on this server"),
    ),
    tag = "magic-link",
)]
async fn redeem_magic_link(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    Query(query): Query<RedeemQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(auth_svc) = state.auth_service.as_ref() else {
        return error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Authentication subsystem is not configured.",
        );
    };

    // PR 22 browser binding: read the per-request challenge from the
    // cookie (set by `POST /api/auth/magic-link/send` on the originating
    // browser). The service compares it to the token's stored
    // challenge. `confirm=1` means the user just clicked through the
    // cross-browser prompt and is fine redeeming from a different
    // browser anyway.
    let incoming_challenge =
        cookie_auth::extract_cookie_value(&headers, cookie_auth::MAGIC_REQUEST_COOKIE);
    let cross_browser_confirmed = query
        .confirm
        .as_deref()
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);

    match auth_svc
        .auth_application_service
        .redeem_magic_link(
            &token,
            incoming_challenge.as_deref(),
            cross_browser_confirmed,
        )
        .await
    {
        Ok(MagicLinkRedeemResult::Allowed(redemption)) => {
            build_success_response(&state, *redemption)
        }
        Ok(MagicLinkRedeemResult::NeedsCrossBrowserConfirm) => {
            cross_browser_confirmation_page(&token)
        }
        Err(e) => {
            // Log the cause for ops; the user gets a generic page so the
            // outcome can't be used as an enumeration oracle.
            tracing::info!(
                target: "audit",
                event = "magic_link.redemption_failed",
                error_kind = ?e.kind,
                error = %e.message,
            );
            match e.kind {
                ErrorKind::NotImplemented => error_page(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Magic-link sign-in is not enabled on this server.",
                ),
                ErrorKind::NotFound | ErrorKind::AccessDenied => error_page(
                    StatusCode::GONE,
                    "This sign-in link is no longer valid. It may have already been \
                     used or expired. Request a fresh link from the login page.",
                ),
                _ => error_page(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong while signing you in. Please try again.",
                ),
            }
        }
    }
}

fn build_success_response(state: &Arc<AppState>, redemption: MagicLinkRedemption) -> Response {
    let target = redirect_target(&redemption);

    let mut response = (StatusCode::FOUND, [(LOCATION, target.as_str())]).into_response();

    cookie_auth::append_auth_cookies(
        response.headers_mut(),
        &redemption.auth.access_token,
        &redemption.auth.refresh_token,
        redemption.auth.expires_in,
        state.core.config.auth.refresh_token_expiry_secs,
    );
    cookie_auth::append_csrf_cookie(response.headers_mut(), redemption.auth.expires_in);
    // Clear the request-challenge cookie — it's single-use and we don't
    // want a stale value on the browser confusing a later flow.
    cookie_auth::append_clear_magic_request_cookie(response.headers_mut());

    response
}

/// Render the cross-browser confirmation page (PR 22). Shown when the
/// magic-link token carries a `request_challenge` (login-via-email)
/// but the inbound cookie didn't match — typically because the user
/// requested the link from one browser and clicked it from another
/// (phone vs desktop, work vs personal). The Continue button submits
/// back to the same endpoint with `?confirm=1` so the service skips
/// the challenge check and proceeds with redemption. Audit-logged at
/// `magic_link.redeemed reason="cross_browser_confirmed"`.
fn cross_browser_confirmation_page(token: &str) -> Response {
    let confirm_url = format!("/magic/v1/{}?confirm=1", html_escape(token));
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <title>Sign in — OxiCloud</title>\
         <style>body{{font-family:system-ui,sans-serif;max-width:520px;margin:6em auto;\
         padding:0 1em;color:#333;line-height:1.5}}\
         h1{{font-size:1.4em}}.btn{{display:inline-block;padding:.7em 1.4em;\
         background:#2563eb;color:#fff;border-radius:6px;text-decoration:none;\
         font-weight:600;margin-top:1em}}.btn:hover{{background:#1d4ed8}}\
         .note{{background:#fef3c7;border-left:3px solid #f59e0b;\
         padding:.75em 1em;margin:1.5em 0;border-radius:4px;font-size:.95em}}</style>\
         </head><body>\
         <h1>Continue signing in on this device?</h1>\
         <p>You opened this sign-in link in a different browser or device than \
         the one where you requested it.</p>\
         <p class=\"note\">If <strong>you</strong> requested this link, it's safe to continue. \
         If you didn't request it, close this page — clicking Continue would sign \
         someone else into your account.</p>\
         <p><a class=\"btn\" href=\"{confirm_url}\">Continue and sign in</a></p>\
         </body></html>",
        confirm_url = confirm_url,
    );

    let mut response = (StatusCode::OK, body).into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
}

/// Build the SPA hash-route the redemption should land on. Mirrors the
/// front-end's `deserializeHash()` parser at `static/js/app/main.js`.
///
/// - **Resource token** (folder invitation): deep-link to the resource.
/// - **NULL-resource token + external user**: land on `/#/sharedwithme`
///   (their entry point — they own no folders themselves).
/// - **NULL-resource token + internal user**: land on `/#/files` (the
///   user has a home folder; the "shared with me" view would be empty
///   on first signup, so home is the better welcome). Internal users
///   on NULL-resource tokens come from the email-only-signup welcome
///   path (PR 18) or from a magic-link they requested themselves
///   while password-eligible-and-lenient-mode (PR 19).
fn redirect_target(redemption: &MagicLinkRedemption) -> String {
    match (redemption.resource_kind, redemption.resource_id) {
        (Some(MagicLinkResourceKind::Folder), Some(folder_id)) => {
            format!("/#/files/folder/{}", folder_id)
        }
        _ if redemption.auth.user.is_external => "/#/sharedwithme".to_string(),
        _ => "/#/files".to_string(),
    }
}

fn error_page(status: StatusCode, message: &str) -> Response {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>OxiCloud</title>\
         <style>body{{font-family:system-ui,sans-serif;max-width:640px;margin:6em auto;\
         padding:0 1em;color:#333}}h1{{font-size:1.4em}}p{{line-height:1.5}}</style>\
         </head><body><h1>Sign-in link</h1><p>{}</p>\
         <p><a href=\"/\">Return to OxiCloud</a></p></body></html>",
        html_escape(message)
    );

    let mut response = (status, body).into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
}

/// Tiny HTML escape — only used in the error fallback page. Anything more
/// elaborate belongs in a templating layer (not in scope here).
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
