//! Finding an app the agent named, so the card shows what is really granted.

use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    pub bundle_id: String,
    pub name: Option<String>,
    pub path: String,
}

impl App {
    pub fn to_json(&self) -> Value {
        json!({"bundleId": self.bundle_id, "name": self.name, "path": self.path})
    }
}

/// The app a query names: a bundle id first, then an exact name, both
/// without case. Nothing for a name two apps share.
pub fn find<'a>(query: &str, apps: &'a [App]) -> Option<&'a App> {
    let query = query.trim();
    if query.is_empty() || query.len() > 255 {
        return None;
    }
    if let Some(app) = apps
        .iter()
        .find(|app| app.bundle_id.eq_ignore_ascii_case(query))
    {
        return Some(app);
    }
    let named: Vec<&App> = apps
        .iter()
        .filter(|app| {
            app.name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(query))
                || app
                    .path
                    .rsplit('/')
                    .next()
                    .and_then(|file| file.strip_suffix(".app"))
                    .is_some_and(|file| file.eq_ignore_ascii_case(query))
        })
        .collect();
    (named.len() == 1).then(|| named[0])
}

/// The apps in the usual folders, read once. macOS only.
#[cfg(target_os = "macos")]
pub fn installed(home: &std::path::Path) -> Vec<App> {
    use std::process::{Command, Stdio};
    let mut folders = vec![
        std::path::PathBuf::from("/Applications"),
        std::path::PathBuf::from("/Applications/Utilities"),
        std::path::PathBuf::from("/System/Applications"),
        std::path::PathBuf::from("/System/Applications/Utilities"),
        std::path::PathBuf::from("/System/Library/CoreServices"),
    ];
    folders.push(home.join("Applications"));
    let mut apps = Vec::new();
    for folder in folders {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok).take(2000) {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("app") {
                continue;
            }
            let plist = path.join("Contents").join("Info.plist");
            // plutil reads binary and XML plists; an argument list, no shell.
            let Ok(output) = Command::new("/usr/bin/plutil")
                .args(["-convert", "json", "-o", "-", "--"])
                .arg(&plist)
                .env_clear()
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
            else {
                continue;
            };
            let Ok(info) = serde_json::from_slice::<Value>(&output.stdout) else {
                continue;
            };
            let Some(bundle_id) = info.get("CFBundleIdentifier").and_then(Value::as_str) else {
                continue;
            };
            let name = info
                .get("CFBundleDisplayName")
                .or_else(|| info.get("CFBundleName"))
                .and_then(Value::as_str)
                .map(|n| n.chars().filter(|c| !c.is_control()).take(80).collect());
            apps.push(App {
                bundle_id: bundle_id.chars().take(255).collect(),
                name,
                path: path.to_string_lossy().into_owned(),
            });
        }
    }
    apps
}

#[cfg(not(target_os = "macos"))]
pub fn installed(_home: &std::path::Path) -> Vec<App> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str, path: &str) -> App {
        App {
            bundle_id: id.into(),
            name: Some(name.into()),
            path: path.into(),
        }
    }

    #[test]
    fn finds_by_bundle_id_then_by_exact_name() {
        let apps = vec![
            app(
                "com.apple.calculator",
                "Calculator",
                "/System/Applications/Calculator.app",
            ),
            app(
                "com.apple.TextEdit",
                "TextEdit",
                "/System/Applications/TextEdit.app",
            ),
            app("com.a.notes", "Notes", "/Applications/Notes.app"),
            app("com.b.notes", "Notes", "/Applications/Notes 2.app"),
        ];
        assert_eq!(
            find("COM.APPLE.CALCULATOR", &apps).unwrap().name.as_deref(),
            Some("Calculator")
        );
        assert_eq!(
            find(" textedit ", &apps).unwrap().bundle_id,
            "com.apple.TextEdit"
        );
        assert!(find("Notes", &apps).is_none(), "two apps share the name");
        assert!(find("Calc", &apps).is_none(), "no partial names");
        assert!(find("", &apps).is_none());
    }
}
