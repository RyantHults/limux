use std::path::PathBuf;

/// Resolve the socket path from CLI flag or environment variable.
pub fn resolve(socket_flag: Option<&str>) -> Result<PathBuf, String> {
    if let Some(path) = socket_flag {
        return Ok(PathBuf::from(path));
    }

    if let Ok(path) = std::env::var("LIMUX_SOCKET") {
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }

    Err("No socket found. Set LIMUX_SOCKET or use --socket PATH.".to_string())
}
