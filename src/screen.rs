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

/// `detect_all` を `spawn_blocking` で実行する。kscreen-doctor の終了待ちで
/// 単一ワーカー（D-Bus・トレイ）を止めないため、非同期側からはこちらを呼ぶ。
pub async fn detect_all_offloaded() -> Vec<Monitor> {
    tokio::task::spawn_blocking(detect_all).await.unwrap_or_else(|e| {
        tracing::error!("screen detection task panicked: {}", e);
        Vec::new()
    })
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
    /// 画面の優先度（1 がプライマリ）。Plasma 5.27 以降、Plasma の画面番号
    /// （スクリプトの `desktop.screen`）はこの順に振られる。
    #[serde(default)]
    priority: Option<u32>,
    /// 優先度が無い古い kscreen の場合のプライマリ指定。
    #[serde(default)]
    primary: bool,
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

/// 有効なモニターを Plasma の画面番号順に返す。
///
/// 返り値の添字はそのまま `plasma::set_wallpaper_multi` の画面番号になるので、
/// kscreen の出力順ではなく優先度順に並べる（プライマリが先頭でないと、
/// 解像度の違うモニターに別の解像度向けの画像を貼ってしまう）。
fn parse_json_monitors(cfg: &KScreenJson) -> Vec<Monitor> {
    let mut outputs: Vec<&JsonOutput> = cfg.outputs.iter().filter(|o| o.enabled).collect();
    // 安定ソートなので、優先度が同じ・無いものは kscreen の順を保つ
    outputs.sort_by_key(|o| (o.priority.filter(|&p| p > 0).unwrap_or(u32::MAX), !o.primary));
    outputs
        .into_iter()
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

    fn json_names(text: &str) -> Vec<String> {
        let parsed: KScreenJson = serde_json::from_str(text).unwrap();
        parse_json_monitors(&parsed).into_iter().map(|m| m.name).collect()
    }

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

    /// Plasma の画面番号は優先度順。kscreen の出力順のまま返すと、
    /// 画面ごとの壁紙が別のモニターに貼られる。
    #[test]
    fn json_orders_monitors_by_priority() {
        let text = r#"{
            "outputs": [
                {"name": "HDMI-1", "enabled": true, "priority": 2, "currentModeId": "a",
                 "modes": [{"id": "a", "size": {"width": 1920, "height": 1080}}]},
                {"name": "DP-1", "enabled": true, "priority": 1, "currentModeId": "b",
                 "modes": [{"id": "b", "size": {"width": 3840, "height": 2160}}]}
            ]
        }"#;
        assert_eq!(json_names(text), ["DP-1", "HDMI-1"]);
    }

    /// 優先度が無い古い kscreen では `primary` を先頭にする。
    #[test]
    fn json_puts_primary_first_without_priority() {
        let text = r#"{
            "outputs": [
                {"name": "HDMI-1", "enabled": true, "currentModeId": "a",
                 "modes": [{"id": "a", "size": {"width": 1920, "height": 1080}}]},
                {"name": "DP-1", "enabled": true, "primary": true, "currentModeId": "b",
                 "modes": [{"id": "b", "size": {"width": 3840, "height": 2160}}]}
            ]
        }"#;
        assert_eq!(json_names(text), ["DP-1", "HDMI-1"]);
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
