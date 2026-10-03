//! 画面解像度の自動取得。
//!
//! `kscreen-doctor --json` の出力をパースして、有効な出力（モニター）の
//! 解像度を返す。環境変数 `KABEKAMI_SCREEN=WxH` が設定されている場合は
//! main.rs 側で優先して使用され、この関数は呼ばれない。

use serde::Deserialize;

/// マルチモニター対応のモニター情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    /// kscreen-doctor が報告するコネクター名（例: "DP-1", "HDMI-1"）。
    pub name: String,
    /// 現在のアクティブ解像度（幅）。
    pub width: u32,
    /// 現在のアクティブ解像度（高さ）。
    pub height: u32,
}

/// 接続・有効化された全モニターを検出する。
///
/// `kscreen-doctor --json` の実行・解析に失敗した場合は空 Vec を返す。
pub fn detect_all() -> Vec<Monitor> {
    let output = match std::process::Command::new("kscreen-doctor").arg("--json").output() {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            tracing::warn!("kscreen-doctor exited with non-zero status: {}", o.status);
            return Vec::new();
        }
        Err(_) => return Vec::new(),
    };
    match serde_json::from_slice::<KScreenJson>(&output.stdout) {
        Ok(parsed) => parse_json_monitors(&parsed),
        Err(e) => {
            tracing::warn!("kscreen-doctor --json parse failed: {}", e);
            Vec::new()
        }
    }
}

// ── JSON パース ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct KScreenJson {
    #[serde(default)]
    outputs: Vec<JsonOutput>,
}

#[derive(Deserialize)]
struct JsonOutput {
    #[serde(default)]
    name: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default, rename = "currentModeId")]
    current_mode_id: Option<String>,
    #[serde(default)]
    modes: Vec<JsonMode>,
}

#[derive(Deserialize)]
struct JsonMode {
    #[serde(default)]
    id: String,
    #[serde(default)]
    size: JsonSize,
}

#[derive(Default, Deserialize)]
struct JsonSize {
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
}

fn parse_json_monitors(cfg: &KScreenJson) -> Vec<Monitor> {
    cfg.outputs
        .iter()
        .filter(|o| o.enabled)
        .filter_map(|o| {
            let Some(current_id) = o.current_mode_id.as_deref() else {
                tracing::warn!(
                    "kscreen JSON: enabled output {:?} has no currentModeId, skipping",
                    o.name
                );
                return None;
            };
            let Some(mode) = o.modes.iter().find(|m| m.id == current_id) else {
                tracing::warn!(
                    "kscreen JSON: enabled output {:?} references unknown currentModeId {:?}, skipping",
                    o.name, current_id
                );
                return None;
            };
            if mode.size.width <= 100 || mode.size.height <= 100 {
                tracing::warn!(
                    "kscreen JSON: enabled output {:?} has implausible size {}x{}, skipping",
                    o.name, mode.size.width, mode.size.height
                );
                return None;
            }
            Some(Monitor {
                name: o.name.clone(),
                width: mode.size.width,
                height: mode.size.height,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_first(text: &str) -> Option<(u32, u32)> {
        let parsed: KScreenJson = serde_json::from_str(text).ok()?;
        parse_json_monitors(&parsed)
            .into_iter()
            .next()
            .map(|m| (m.width, m.height))
    }

    #[test]
    fn json_parses_current_mode() {
        let text = r#"{
            "outputs": [{
                "name": "DP-1",
                "enabled": true,
                "currentModeId": "mode-2",
                "modes": [
                    {"id": "mode-1", "size": {"width": 1920, "height": 1080}},
                    {"id": "mode-2", "size": {"width": 2560, "height": 1440}}
                ]
            }]
        }"#;
        assert_eq!(json_first(text), Some((2560, 1440)));
    }

    #[test]
    fn json_skips_disabled() {
        let text = r#"{
            "outputs": [
                {"name": "DP-1", "enabled": false, "currentModeId": "x",
                 "modes": [{"id": "x", "size": {"width": 1920, "height": 1080}}]},
                {"name": "HDMI-1", "enabled": true, "currentModeId": "y",
                 "modes": [{"id": "y", "size": {"width": 3840, "height": 2160}}]}
            ]
        }"#;
        assert_eq!(json_first(text), Some((3840, 2160)));
    }

    #[test]
    fn json_skips_when_current_mode_missing() {
        let text = r#"{
            "outputs": [{
                "name": "DP-1", "enabled": true, "currentModeId": "missing",
                "modes": [{"id": "other", "size": {"width": 1920, "height": 1080}}]
            }]
        }"#;
        assert_eq!(json_first(text), None);
    }

    #[test]
    fn json_handles_empty_outputs() {
        let text = r#"{"outputs": []}"#;
        let parsed: KScreenJson = serde_json::from_str(text).unwrap();
        assert!(parse_json_monitors(&parsed).is_empty());
    }

    #[test]
    fn json_parses_multiple_enabled() {
        let text = r#"{
            "outputs": [
                {"name": "DP-1", "enabled": true, "currentModeId": "a",
                 "modes": [{"id": "a", "size": {"width": 2560, "height": 1440}}]},
                {"name": "HDMI-1", "enabled": true, "currentModeId": "b",
                 "modes": [{"id": "b", "size": {"width": 1920, "height": 1080}}]}
            ]
        }"#;
        let parsed: KScreenJson = serde_json::from_str(text).unwrap();
        let monitors = parse_json_monitors(&parsed);
        assert_eq!(monitors.len(), 2);
        assert_eq!(monitors[0].name, "DP-1");
        assert_eq!(monitors[1].name, "HDMI-1");
    }
}
