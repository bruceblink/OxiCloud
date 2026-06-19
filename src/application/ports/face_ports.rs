//! Ports for the People (faces) feature.

use async_trait::async_trait;
use uuid::Uuid;

use crate::common::errors::DomainError;
use crate::domain::entities::face::{DetectedFace, Face, Person};

/// Detects faces in an image and produces an aligned, L2-normalized embedding
/// for each. Takes raw encoded bytes (it decodes internally) so the
/// application layer stays decoupled from any image/ML crate.
///
/// The default implementation ([`NoopFaceAnalyzer`](crate::infrastructure::services::noop_face_analyzer::NoopFaceAnalyzer))
/// is a no-op that reports `is_ready() == false`; a real ONNX-backed
/// implementation is wired in when the operator provides models at runtime.
#[async_trait]
pub trait FaceAnalyzerPort: Send + Sync + 'static {
    /// Whether a usable model is loaded. When false, indexing is skipped.
    fn is_ready(&self) -> bool;

    /// Detect and embed every face in `image_bytes` (an encoded JPEG/PNG/…).
    async fn analyze(&self, image_bytes: &[u8]) -> Result<Vec<DetectedFace>, DomainError>;
}

/// Persistence for faces and persons. Every method is user-scoped; the
/// repository enforces `WHERE user_id = …` so callers only ever touch their
/// own biometric data.
#[async_trait]
pub trait FaceRepository: Send + Sync + 'static {
    // ── faces ──────────────────────────────────────────────────────
    async fn save_faces(&self, faces: &[Face]) -> Result<(), DomainError>;
    async fn faces_for_file(&self, file_id: Uuid) -> Result<Vec<Face>, DomainError>;
    async fn delete_faces_for_file(&self, file_id: Uuid) -> Result<(), DomainError>;
    async fn faces_for_user(&self, user_id: Uuid) -> Result<Vec<Face>, DomainError>;
    /// Faces previously computed for any file sharing this content hash —
    /// lets indexing reuse results for deduplicated (identical) uploads.
    async fn faces_for_blob(
        &self,
        user_id: Uuid,
        blob_hash: &str,
    ) -> Result<Vec<Face>, DomainError>;
    async fn assign_person(
        &self,
        face_id: Uuid,
        person_id: Option<Uuid>,
    ) -> Result<(), DomainError>;

    // ── persons ────────────────────────────────────────────────────
    async fn create_person(&self, person: &Person) -> Result<(), DomainError>;
    async fn persons_for_user(&self, user_id: Uuid) -> Result<Vec<Person>, DomainError>;
    async fn rename_person(
        &self,
        user_id: Uuid,
        person_id: Uuid,
        name: Option<String>,
    ) -> Result<(), DomainError>;
    async fn set_person_cover(
        &self,
        person_id: Uuid,
        cover_face_id: Uuid,
    ) -> Result<(), DomainError>;
    async fn set_person_hidden(
        &self,
        user_id: Uuid,
        person_id: Uuid,
        hidden: bool,
    ) -> Result<(), DomainError>;
    /// File ids that contain a face assigned to this person (most recent first).
    async fn files_for_person(
        &self,
        user_id: Uuid,
        person_id: Uuid,
    ) -> Result<Vec<Uuid>, DomainError>;

    /// Hard-delete every face and person for a user (right to erasure /
    /// disabling the feature).
    async fn delete_all_for_user(&self, user_id: Uuid) -> Result<(), DomainError>;
}
