//! Outbound email port.
//!
//! Single-recipient transactional mail sending — the entry point for the
//! magic-link invitation flow (PR 9), the login-via-email flow (PR 10), and
//! any future notification mail. Kept deliberately small: one method, one
//! recipient, one body pair (text + optional HTML).
//!
//! The infrastructure-layer implementation lives at
//! `src/infrastructure/services/smtp_email_sender.rs` and is constructed
//! lazily in [`AppServiceFactory`]: when `OXICLOUD_SMTP_HOST` is empty the
//! DI container holds `None`, and endpoints that require email return a
//! clear 503 ("SMTP not configured") rather than silently dropping mail.
//!
//! Future evolution: an in-memory `MemoryEmailSender` for tests (no SMTP
//! round-trip), and a `LoggingEmailSender` decorator that records every
//! send to the audit log. Both are deferred until a concrete consumer
//! needs them.

use async_trait::async_trait;

use crate::common::errors::DomainError;

/// A single outbound message. The `to` address is expected to be a normalised
/// RFC 5321 mailbox (lowercase local-part + punycoded domain); upstream
/// callers handle the normalisation before constructing this struct.
#[derive(Debug, Clone)]
pub struct EmailMessage {
    /// RFC 5321 recipient address. Single recipient per send today — the
    /// invite flow targets one external user at a time. Multi-recipient
    /// (CC/BCC) is intentionally out of scope.
    pub to: String,
    /// Plain-text subject line. UTF-8 — lettre handles RFC 2047 encoding.
    pub subject: String,
    /// Plain-text body. Always required; mail clients without HTML
    /// rendering fall back to this.
    pub text_body: String,
    /// Optional HTML body. When present, the message is sent as
    /// `multipart/alternative` with both representations.
    pub html_body: Option<String>,
}

/// Port for sending transactional email.
///
/// Implementations must:
/// - Be idempotent at the network level (lettre handles connection reuse).
/// - Run the actual SMTP exchange on the existing tokio runtime (no
///   blocking threads).
/// - Return `DomainError` with `ErrorKind::ExternalService` (or the most
///   precise variant available) on permanent failures so handlers can
///   distinguish "couldn't reach SMTP" from validation errors.
///
/// `#[async_trait]` is used so the trait is dyn-compatible — the DI
/// container holds `Arc<dyn EmailSender>` (matches the existing
/// `dyn` patterns at the service boundary).
#[async_trait]
pub trait EmailSender: Send + Sync + 'static {
    /// Send one message. Returns `Ok(())` only after the SMTP server has
    /// accepted the message (i.e. after the final `.` or LMTP DATA close).
    /// Caller may run this fire-and-forget via `tokio::spawn` if response
    /// timing matters (e.g. magic-link invite path defending against
    /// enumeration via latency).
    async fn send(&self, message: EmailMessage) -> Result<(), DomainError>;
}
