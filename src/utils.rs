use arboard::Clipboard;
use std::fs;
use std::io::{self, Read, Write};

pub fn emit(text: &str, copy: bool) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    // A reader that stops early, like `| head`, isn't an error.
    if let Err(e) = writeln!(io::stdout(), "{text}")
        && e.kind() != io::ErrorKind::BrokenPipe
    {
        return Err(format!("failed to write the output: {e}"));
    }
    if copy {
        copy_to_clipboard(text)?;
    }
    Ok(())
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard = Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    clipboard
        .set_text(text.to_string())
        .map_err(|e| format!("failed to copy to clipboard: {e}"))
}

pub fn read_stdin_or_file(input_file: &Option<String>) -> Result<String, String> {
    if let Some(file) = input_file {
        fs::read_to_string(file).map_err(|e| format!("Failed to read from file: {}", e))
    } else {
        let mut buffer = String::new();
        io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|e| format!("Failed to read from stdin: {}", e))?;
        Ok(buffer)
    }
}

/// Lowercase hex, as checksums are usually written.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
