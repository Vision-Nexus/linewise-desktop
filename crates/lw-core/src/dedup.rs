use crate::db::Database;
use crate::error::AppError;
use std::path::Path;

/// Compute BLAKE3 hash of a file
pub async fn hash_file(path: &Path) -> Result<String, std::io::Error> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut hasher = blake3::Hasher::new();
        let file = std::fs::File::open(&path)?;
        let mut reader = std::io::BufReader::with_capacity(1024 * 1024, file);
        std::io::copy(&mut reader, &mut hasher)?;
        Ok(hasher.finalize().to_hex().to_string())
    })
    .await
    .expect("hash_file task panicked")
}

/// Check if a file is a duplicate based on its hash
pub async fn check_duplicate(db: &Database, path: &Path) -> Result<Option<String>, AppError> {
    let hash = hash_file(path)
        .await
        .map_err(|e| AppError::Upload(crate::error::UploadError::Io(e)))?;
    db.find_by_hash(&hash).await.map_err(AppError::Database)
}
