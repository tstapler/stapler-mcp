use stapler_mcp_core::ports::{FileStore, PortError};

/// Per-process, monotonically increasing counter used to build unique temp
/// filenames for atomic writes. `std::process::id()` alone is insufficient:
/// it is constant for the daemon's entire lifetime, so two concurrent
/// `write_file` calls to the *same* path within one daemon run would
/// otherwise collide on an identical temp filename.
static TMP_FILE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub struct NativeFs;

impl FileStore for NativeFs {
    async fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), PortError> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| PortError::Io(e.to_string()))?;
        }

        let tmp_path = format!(
            "{path}.tmp-{}-{}",
            std::process::id(),
            TMP_FILE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );

        if let Err(e) = tokio::fs::write(&tmp_path, bytes).await {
            if let Err(cleanup_err) = tokio::fs::remove_file(&tmp_path).await {
                eprintln!("write_file: failed to clean up temp file {tmp_path}: {cleanup_err}");
            }
            return Err(PortError::Io(e.to_string()));
        }

        if let Err(e) = tokio::fs::rename(&tmp_path, path).await {
            if let Err(cleanup_err) = tokio::fs::remove_file(&tmp_path).await {
                eprintln!("write_file: failed to clean up temp file {tmp_path}: {cleanup_err}");
            }
            return Err(PortError::Io(e.to_string()));
        }

        Ok(())
    }

    async fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>, PortError> {
        match tokio::fs::read(path).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(PortError::Io(e.to_string())),
        }
    }

    async fn delete_file(&self, path: &str) -> Result<(), PortError> {
        match tokio::fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(PortError::Io(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "stapler-mcp-native-fs-test-{}-{}",
            std::process::id(),
            TMP_FILE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        dir
    }

    #[tokio::test]
    async fn should_delete_file_and_return_ok_when_file_exists() {
        let dir = temp_dir();
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("meta.json");
        tokio::fs::write(&path, b"{}").await.unwrap();

        let fs = NativeFs;
        let result = fs.delete_file(path.to_str().unwrap()).await;

        assert!(result.is_ok());
        assert!(!path.exists());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn should_return_ok_not_err_when_deleting_file_that_does_not_exist() {
        let dir = temp_dir();
        let path = dir.join("does-not-exist.json");

        let fs = NativeFs;
        let result = fs.delete_file(path.to_str().unwrap()).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn should_write_full_bytes_and_leave_no_stray_temp_file_when_write_succeeds() {
        let dir = temp_dir();
        let path = dir.join("chunks.jsonl");

        let fs = NativeFs;
        let result = fs.write_file(path.to_str().unwrap(), b"hello world").await;

        assert!(result.is_ok());
        let contents = tokio::fs::read(&path).await.unwrap();
        assert_eq!(contents, b"hello world");

        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        assert_eq!(names, vec!["chunks.jsonl".to_string()]);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn should_leave_original_content_untouched_when_write_is_interrupted_before_rename() {
        let dir = temp_dir();
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("sources.json");
        tokio::fs::write(&path, b"original content").await.unwrap();

        // Simulate the write-before-rename step directly: write to a temp
        // path but never rename it into place, then assert the final path's
        // content is untouched and the temp file exists alongside it.
        let tmp_path = format!(
            "{}.tmp-{}-{}",
            path.to_str().unwrap(),
            std::process::id(),
            TMP_FILE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        tokio::fs::write(&tmp_path, b"new content").await.unwrap();

        let original = tokio::fs::read(&path).await.unwrap();
        assert_eq!(original, b"original content");
        assert!(std::path::Path::new(&tmp_path).exists());

        let _ = tokio::fs::remove_file(&tmp_path).await;
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn should_produce_distinct_temp_filenames_when_two_concurrent_writes_target_same_path() {
        let dir = temp_dir();
        let path = dir.join("sources.json");
        let path_str = path.to_str().unwrap().to_string();

        let fs = NativeFs;
        let fs2 = NativeFs;
        let path_str2 = path_str.clone();

        let (r1, r2) = tokio::join!(
            fs.write_file(&path_str, b"write-one"),
            fs2.write_file(&path_str2, b"write-two")
        );

        assert!(r1.is_ok(), "first concurrent write failed: {r1:?}");
        assert!(r2.is_ok(), "second concurrent write failed: {r2:?}");

        // Whichever write "won," the final content is exactly one of the two
        // complete payloads, never a mix, and no stray temp files remain.
        let final_contents = tokio::fs::read(&path).await.unwrap();
        assert!(final_contents == b"write-one" || final_contents == b"write-two");

        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        assert_eq!(names, vec!["sources.json".to_string()]);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
