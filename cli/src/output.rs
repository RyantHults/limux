use std::process;

/// Handle a command result: print output and exit appropriately.
pub fn handle(result: Result<String, String>, json: bool) {
    match result {
        Ok(data) => {
            if json {
                if data.is_empty() {
                    println!("{{\"ok\":true,\"data\":null}}");
                } else {
                    let escaped = serde_json::to_string(&data).unwrap_or_default();
                    println!("{{\"ok\":true,\"data\":{escaped}}}");
                }
            } else if !data.is_empty() {
                println!("{data}");
            }
        }
        Err(msg) => {
            if json {
                let escaped = serde_json::to_string(&msg).unwrap_or_default();
                println!("{{\"ok\":false,\"error\":{escaped}}}");
            } else {
                eprintln!("Error: {msg}");
            }
            process::exit(1);
        }
    }
}
