use arboard::Clipboard;
use std::fs;
use std::io::{self, Read};

pub fn emit(text: &str, copy: bool) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    println!("{text}");
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
