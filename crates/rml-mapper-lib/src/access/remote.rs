use super::Access;
use crate::error::{Result, RmlError};
use std::io::{Cursor, Read};

use super::LocalFileAccess;
/// Access to remote HTTP/HTTPS resources
use std::path::Path;

///
/// Provides access to remote files via HTTP/HTTPS using reqwest.
///
/// # Examples
///
/// ```
/// use rml_mapper::access::RemoteFileAccess;
///
/// let access = RemoteFileAccess::new(
///     "https://example.org/data.csv".to_string(),
///     Some("text/csv".to_string())
/// );
/// ```
#[derive(Debug, Clone)]
pub struct RemoteFileAccess {
    /// URL of the remote resource
    url: String,
    /// Content type/MIME type
    content_type: Option<String>,
}

impl RemoteFileAccess {
    /// Creates a new remote file access
    ///
    /// # Arguments
    ///
    /// * `url` - URL of the remote resource
    /// * `content_type` - Optional content type/MIME type
    pub fn new(url: String, content_type: Option<String>) -> Self {
        Self { url, content_type }
    }

    /// Detects content type from URL extension
    pub(crate) fn detect_content_type_from_url(url: &str) -> Option<String> {
        url.split('?')
            .next()
            .and_then(|path| Path::new(path).extension())
            .and_then(|ext| ext.to_str())
            .and_then(|ext| {
                LocalFileAccess::detect_content_type(Path::new(&format!("file.{}", ext)))
            })
    }
}

impl Access for RemoteFileAccess {
    fn get_reader(&self) -> Result<Box<dyn Read + Send>> {
        let response = reqwest::blocking::get(&self.url)
            .map_err(|e| RmlError::Http(format!("Failed to fetch URL '{}': {}", self.url, e)))?;

        if !response.status().is_success() {
            return Err(RmlError::Http(format!(
                "HTTP request failed with status {}: {}",
                response.status(),
                self.url
            )));
        }

        let bytes = response.bytes().map_err(|e| {
            RmlError::Http(format!(
                "Failed to read response body from '{}': {}",
                self.url, e
            ))
        })?;

        Ok(Box::new(Cursor::new(bytes.to_vec())))
    }

    fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    fn cache_key(&self) -> String {
        self.url.clone()
    }
}
