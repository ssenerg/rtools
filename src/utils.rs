use arboard::Clipboard;

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
