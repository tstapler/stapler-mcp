use stapler_mcp_core::ports::{FileStore, PortError};

pub struct NativeFs;

impl FileStore for NativeFs {
    async fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), PortError> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| PortError::Io(e.to_string()))?;
        }
        tokio::fs::write(path, bytes)
            .await
            .map_err(|e| PortError::Io(e.to_string()))
    }

    async fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>, PortError> {
        match tokio::fs::read(path).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(PortError::Io(e.to_string())),
        }
    }
}
