//! Default no-op face analyzer.
//!
//! Used when no ML model is configured: it reports `is_ready() == false` and
//! returns no faces, so the whole People pipeline compiles and runs inert
//! until a real ONNX-backed analyzer (provided by the operator) replaces it.

use async_trait::async_trait;

use crate::application::ports::face_ports::FaceAnalyzerPort;
use crate::common::errors::DomainError;
use crate::domain::entities::face::DetectedFace;

/// Analyzer that never detects anything.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopFaceAnalyzer;

#[async_trait]
impl FaceAnalyzerPort for NoopFaceAnalyzer {
    fn is_ready(&self) -> bool {
        false
    }

    async fn analyze(&self, _image_bytes: &[u8]) -> Result<Vec<DetectedFace>, DomainError> {
        Ok(Vec::new())
    }
}
