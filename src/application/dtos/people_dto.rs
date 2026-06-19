//! DTOs for the People (faces) API.

use serde::Serialize;
use utoipa::ToSchema;

/// A named (or unnamed) identity cluster, with a cover photo for its tile.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PersonDto {
    pub id: String,
    /// `None` until the user names the person.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// File id of the cover face's photo, for the tile thumbnail.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_file_id: Option<String>,
    pub face_count: i64,
    pub is_hidden: bool,
}

/// One face box within a photo (for tagging overlays in the lightbox).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FaceBoxDto {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person_id: Option<String>,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}
