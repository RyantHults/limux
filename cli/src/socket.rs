use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// Send a command to the limux socket and return the response data.
/// Returns `Ok(data)` for `OK [data]` responses, `Err(msg)` for errors.
pub fn send_command(socket_path: &Path, command: &str) -> Result<String, String> {
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|e| format!("Failed to connect to {}: {e}", socket_path.display()))?;

    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .ok();

    writeln!(stream, "{command}")
        .map_err(|e| format!("Failed to send command: {e}"))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("Failed to read response: {e}"))?;

    let line = line.trim_end();

    // Length-prefixed multi-line response: "OK+<len>"
    if let Some(len_str) = line.strip_prefix("OK+") {
        let len: usize = len_str.parse()
            .map_err(|_| format!("Invalid length in response: {len_str}"))?;
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf)
            .map_err(|e| format!("Failed to read response body: {e}"))?;
        return Ok(String::from_utf8_lossy(&buf).into_owned());
    }

    if let Some(data) = line.strip_prefix("OK") {
        Ok(data.trim_start().to_string())
    } else if let Some(msg) = line.strip_prefix("ERROR: ") {
        Err(msg.to_string())
    } else {
        Err(format!("Unexpected response: {line}"))
    }
}
