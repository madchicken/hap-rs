use std::{error::Error as StdError, sync::Arc};

use futures::future::BoxFuture;
use serde::Deserialize;

/// A `POST /resource` request: iOS asks a camera accessory for a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResourceRequest {
    /// Accessory the snapshot is for (absent for single accessory servers).
    pub aid: Option<u64>,
    /// Requested resource, `image` for snapshots.
    #[serde(rename = "resource-type")]
    pub resource_type: String,
    /// Preferred width of the image.
    #[serde(rename = "image-width")]
    pub image_width: u32,
    /// Preferred height of the image.
    #[serde(rename = "image-height")]
    pub image_height: u32,
}

/// Produces the JPEG bytes answering a [`ResourceRequest`].
pub type ResourceHandler = Arc<
    dyn Fn(ResourceRequest) -> BoxFuture<'static, std::result::Result<Vec<u8>, Box<dyn StdError + Send + Sync>>>
        + Send
        + Sync,
>;
