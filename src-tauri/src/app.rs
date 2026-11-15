use serde::Serialize;
use tauri::State;
use ts_rs::TS;

use crate::state::AppState;

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub platform: String,
    pub arch: String,
    pub debug_build: bool,
    pub data_dir: String,
    /// The Typst compiler built into the application (e.g. "typst 0.15.1").
    pub typst_compiler: String,
    /// The Markdown print profile (Markdown is laid out through Typst).
    pub markdown_profile: String,
}

#[tauri::command]
pub async fn app_info(state: State<'_, AppState>) -> Result<AppInfo, crate::CommandError> {
    Ok(AppInfo {
        name: "Exposition".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        platform: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        debug_build: cfg!(debug_assertions),
        data_dir: state.data_dir.display().to_string(),
        typst_compiler: exposition_typesetting::typst_adapter::COMPILER.into(),
        markdown_profile: exposition_typesetting::markdown::PROFILE_ID.into(),
    })
}

/// Longest URL `app_open_external` accepts.
const MAX_EXTERNAL_URL: usize = 2048;

/// Validate a URL the user chose to open in the system browser: absolute
/// `http`/`https` with a host, no embedded credentials, no whitespace or
/// control characters, at most 2048 characters. Anything else (`javascript:`,
/// `file:`, `data:`, relative links, app-internal schemes) is refused.
pub fn check_external_url(raw: &str) -> Result<tauri::Url, crate::CommandError> {
    let refuse = |why: &str| crate::CommandError::new("app.invalidUrl", format!("This link cannot be opened: {why}."));
    if raw.len() > MAX_EXTERNAL_URL {
        return Err(refuse("it is longer than 2048 characters"));
    }
    if raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(refuse("it contains spaces or control characters"));
    }
    // Invisible and direction-changing characters would make the shown
    // address differ from the one opened.
    if raw.chars().any(|c| {
        matches!(c, '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
    }) {
        return Err(refuse("it contains invisible or direction-changing characters"));
    }
    let url = tauri::Url::parse(raw).map_err(|_| refuse("it is not a complete web address"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(refuse("only http and https links open in the browser"));
    }
    let host = url.host_str().unwrap_or("");
    if host.is_empty() {
        return Err(refuse("it has no host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(refuse("it contains a user name or password"));
    }
    Ok(url)
}

/// Open a web link in the system's default browser. Only called after the
/// user chose a link shown with its full address (rendered agent responses);
/// the app's own window never navigates away (see [`navigation_guard`]).
#[tauri::command]
pub async fn app_open_external(url: String) -> Result<(), crate::CommandError> {
    let url = check_external_url(&url)?;
    open_in_browser(url.as_str())
        .map_err(|e| crate::CommandError::new("app.openFailed", format!("The browser could not be started: {e}")))
}

#[cfg(windows)]
fn open_in_browser(url: &str) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    /// `SW_SHOWNORMAL`.
    const SW_SHOWNORMAL: i32 = 1;
    let wide = |s: &str| std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let verb = wide("open");
    let target = wide(url);
    // SAFETY: both strings are NUL-terminated UTF-16 buffers that outlive the call.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW reports success with a value greater than 32.
    if result as isize > 32 {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("ShellExecute returned {}", result as isize)))
    }
}

#[cfg(target_os = "macos")]
fn open_in_browser(url: &str) -> std::io::Result<()> {
    std::process::Command::new("open").arg(url).spawn().map(|_| ())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_in_browser(url: &str) -> std::io::Result<()> {
    std::process::Command::new("xdg-open").arg(url).spawn().map(|_| ())
}

/// Whether the main webview may navigate to `url`: only the application's own
/// content (and, in debug builds, the Vite dev server). A link in rendered
/// content can never take the window to another page.
pub fn navigation_allowed(url: &tauri::Url) -> bool {
    match url.scheme() {
        "tauri" => true,
        "http" | "https" => {
            let host = url.host_str().unwrap_or("");
            host == "tauri.localhost" || (cfg!(debug_assertions) && host == "localhost" && url.port() == Some(1420))
        }
        "about" => url.as_str() == "about:blank",
        _ => false,
    }
}

/// A plugin whose only job is refusing navigation away from the app.
pub fn navigation_guard<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("navigation-guard")
        .on_navigation(|_webview, url| {
            let allowed = navigation_allowed(url);
            if !allowed {
                eprintln!("[exposition] blocked navigation to a {} URL", url.scheme());
            }
            allowed
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_urls_are_http_or_https_with_a_host() {
        assert!(check_external_url("https://arxiv.org/abs/2401.00001").is_ok());
        assert!(check_external_url("http://example.org/a?b=c#d").is_ok());
        for bad in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            "file:///C:/Windows/win.ini",
            "data:text/html,<script>1</script>",
            "tauri://localhost/index.html",
            "ipc://localhost/agents_task_create",
            "/relative/path",
            "notes.md",
            "https://user:pw@example.org/",
            "https://example.org/a b",
            "https://example.org/\u{202e}",
            "https://exa\u{200b}mple.org/",
            "mailto:someone@example.org",
            "",
        ] {
            assert!(check_external_url(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(check_external_url(&format!("https://example.org/{}", "a".repeat(2100))).is_err());
    }

    #[test]
    fn the_window_navigates_only_within_the_app() {
        let ok = |s: &str| navigation_allowed(&tauri::Url::parse(s).unwrap());
        assert!(ok("tauri://localhost/index.html"));
        assert!(ok("http://tauri.localhost/index.html"));
        assert!(ok("about:blank"));
        assert!(!ok("https://example.org/"));
        assert!(!ok("http://tauri.localhost.evil.example/"));
        assert!(!ok("file:///C:/Windows/win.ini"));
        assert!(!ok("about:srcdoc"));
        assert_eq!(ok("http://localhost:1420/"), cfg!(debug_assertions));
        assert!(!ok("http://localhost:8080/"));
    }
}
