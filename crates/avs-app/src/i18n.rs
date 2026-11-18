//! UI language. The App Shell Foundation owns the preference; this is only the
//! resolved value plus a helper that picks one of two literal strings, so every
//! label is written next to its translation.

use crate::app_shell_foundation::AppShellLanguage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    Ja,
    En,
}

impl Lang {
    pub fn pick(self, ja: &'static str, en: &'static str) -> &'static str {
        match self {
            Lang::Ja => ja,
            Lang::En => en,
        }
    }

    pub fn from_shell(language: AppShellLanguage) -> Lang {
        match language {
            AppShellLanguage::Japanese => Lang::Ja,
            AppShellLanguage::English => Lang::En,
        }
    }
}

/// The macOS system language: the first entry of `AppleLanguages`, else `LANG`.
pub fn system_language() -> AppShellLanguage {
    let from_defaults = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLanguages"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).to_string());
    let first = from_defaults
        .as_deref()
        .and_then(|text| text.split(['"', '\n', ',']).map(str::trim).find(|part| part.len() >= 2 && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')))
        .map(str::to_string)
        .or_else(|| std::env::var("LANG").ok());
    match first {
        Some(tag) if tag.to_ascii_lowercase().starts_with("ja") => AppShellLanguage::Japanese,
        Some(_) => AppShellLanguage::English,
        None => AppShellLanguage::Japanese,
    }
}

/// What an edit did, in Japanese, for the status bar. The operation's own summary
/// is written in English for agents; `None` means there is no translation and the
/// caller shows that summary.
pub fn edit_label_ja(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "add_track" => "トラックを追加しました",
        "reorder_track" => "トラックを並べ替えました",
        "delete_track" => "トラックを削除しました",
        "set_track_muted" => "トラックのミュートを切り替えました",
        "set_track_properties" => "トラックの設定を変更しました",
        "register_media" => "素材を登録しました",
        "delete_media" => "素材を削除しました",
        "add_clips" => "クリップを追加しました",
        "move_clip" | "move_clips" => "クリップを移動しました",
        "trim_clip" | "ripple_trim_clip" | "trim_clip_to_time" | "trim_clips_to_time" => "クリップをトリムしました",
        "roll_edit" => "編集点を動かしました",
        "slip_clip" => "クリップをスリップしました",
        "slide_clip" => "クリップをスライドしました",
        "delete_left_at_time" | "delete_clips_left_at_time" => "再生ヘッドより前を削除しました",
        "delete_right_at_time" | "delete_clips_right_at_time" => "再生ヘッドより後ろを削除しました",
        "split_clip" | "blade_at_time" => "クリップを分割しました",
        "delete_clip" | "delete_clips" => "クリップを削除しました",
        "ripple_delete_clip" => "クリップを詰めて削除しました",
        "close_gap_at_time" => "隙間を詰めました",
        "lift_range" => "範囲を削除しました",
        "extract_range" => "範囲を詰めて削除しました",
        "set_clip_properties" => "クリップの設定を変更しました",
        "set_clip_audio" => "クリップの音声を変更しました",
        "set_caption_text" => "字幕を変更しました",
        "add_captions" => "字幕を追加しました",
        "set_project_settings" => "プロジェクトの設定を変更しました",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every edit the catalog publishes has a Japanese status line, so the status
    /// bar never falls back to the English summary in a Japanese window.
    #[test]
    fn every_edit_tool_has_a_japanese_label() {
        let catalog: serde_json::Value = serde_json::from_str(include_str!("../../avs-mcp/tools.json")).unwrap();
        let tools = catalog.as_array().or_else(|| catalog.get("tools").and_then(|tools| tools.as_array())).unwrap();
        let mut checked = 0;
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            let operation = serde_json::json!({ "tool": name, "args": {} });
            // Only names that are operations matter; the rest are not edits.
            let is_operation = match serde_json::from_value::<avs_core::Operation>(operation) {
                Ok(_) => true,
                Err(error) => !error.to_string().contains("unknown variant"),
            };
            if is_operation {
                checked += 1;
                assert!(edit_label_ja(name).is_some(), "no Japanese status for {name}");
            }
        }
        assert!(checked >= 30, "only {checked} operations were recognised");
    }
}
