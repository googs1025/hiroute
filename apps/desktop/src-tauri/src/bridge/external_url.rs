use crate::failure::DesktopFailure;
use tauri::WebviewWindow;

use super::main_window;

const MAX_EXTERNAL_URL_BYTES: usize = 2_048;

fn validated_external_http_url(url: &str) -> Result<&str, DesktopFailure> {
    if url.is_empty()
        || url.len() > MAX_EXTERNAL_URL_BYTES
        || url
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err("EXTERNAL_URL_INVALID".into());
    }
    let authority = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or("EXTERNAL_URL_SCHEME_DENIED")?
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err("EXTERNAL_URL_INVALID".into());
    }
    Ok(url)
}

#[tauri::command]
pub async fn open_external_url(window: WebviewWindow, url: String) -> Result<(), DesktopFailure> {
    main_window(&window)?;
    let url = validated_external_http_url(&url)?.to_owned();
    tauri::async_runtime::spawn_blocking(move || open_with_platform_browser(&url))
        .await
        .map_err(|_| DesktopFailure::from("EXTERNAL_URL_OPEN_FAILED"))?
}

fn open_with_platform_browser(url: &str) -> Result<(), DesktopFailure> {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "linux")]
    let mut command = std::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = url;
        return Err("EXTERNAL_URL_OPEN_FAILED".into());
    }
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    command
        .arg(url)
        .spawn()
        .map(|_child| ())
        .map_err(|_| "EXTERNAL_URL_OPEN_FAILED".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_url_accepts_only_bounded_http_urls_with_an_authority() {
        assert_eq!(
            validated_external_http_url("https://example.com/guide?q=key").unwrap(),
            "https://example.com/guide?q=key"
        );
        assert_eq!(
            validated_external_http_url("http://127.0.0.1:8080/docs").unwrap(),
            "http://127.0.0.1:8080/docs"
        );
        for denied in [
            "file:///tmp/private",
            "javascript:alert(1)",
            "custom://example.com",
            "https://user@example.com/private",
            "https://",
            "https://example.com/line\nbreak",
        ] {
            assert!(validated_external_http_url(denied).is_err(), "{denied}");
        }
        assert!(
            validated_external_http_url(&format!(
                "https://example.com/{}",
                "x".repeat(MAX_EXTERNAL_URL_BYTES)
            ))
            .is_err()
        );
    }
}
