//! 画面解像度の自動取得。
//!
//! `kscreen-doctor --outputs` の出力をパースして、有効な出力（モニター）の
//! 解像度を返す。環境変数 `KABEKAMI_SCREEN=WxH` が設定されている場合は
//! main.rs 側で優先して使用され、この関数は呼ばれない。
//!
//! ## kscreen-doctor 出力例
//!
//! ```text
//! Output: 1 DP-1 enabled connected primary geometry 0,0,2560x1440 resolution 2560x1440@60
//! Output: 2 HDMI-1 disabled disconnected
//! ```
//!
//! または（別バージョン）:
//!
//! ```text
//! Output: 1 eDP-1 enabled connected primary
//!   modes:
//!     1: 1920x1080@60 *current
//! ```

/// マルチモニター対応のモニター情報。
#[derive(Debug, Clone)]
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
/// `kscreen-doctor --outputs` を実行し、`enabled` な全出力の情報を返す。
/// コマンドが利用できない / 失敗した場合は空 Vec を返す。
pub fn detect_all() -> Vec<Monitor> {
    let output = std::process::Command::new("kscreen-doctor")
        .arg("--outputs")
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        Ok(_) => {
            tracing::warn!("kscreen-doctor exited with non-zero status");
            return Vec::new();
        }
        Err(_) => return Vec::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    tracing::debug!("kscreen-doctor output:\n{}", stdout);
    parse_all_monitors(&stdout)
}

/// テキストから全 `enabled` 出力の解像度とコネクター名を取り出す。
fn parse_all_monitors(text: &str) -> Vec<Monitor> {
    let mut monitors = Vec::new();
    let mut in_enabled = false;
    let mut current_name = String::new();
    let mut candidate: Option<(u32, u32)> = None;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("Output:") {
            if in_enabled {
                if let Some((w, h)) = candidate.take() {
                    monitors.push(Monitor { name: current_name.clone(), width: w, height: h });
                }
            }
            candidate = None;
            in_enabled = trimmed.contains("enabled");
            let tokens: Vec<&str> = trimmed.split_whitespace().collect();
            current_name = tokens.get(2).copied().unwrap_or("").to_string();
        }

        if !in_enabled {
            continue;
        }

        for token in trimmed.split(|c: char| c.is_ascii_whitespace() || c == ',') {
            let base = token.split('@').next().unwrap_or(token);
            if let Some(res) = parse_wxh(base) {
                if trimmed.contains("*current") {
                    candidate = Some(res);
                } else {
                    candidate.get_or_insert(res);
                }
                break;
            }
        }
    }

    if in_enabled {
        if let Some((w, h)) = candidate {
            monitors.push(Monitor { name: current_name, width: w, height: h });
        }
    }

    monitors
}

/// "WxH" 文字列を `(width, height)` にパースする。
/// `w > 100 && h > 100` でなければ座標値と誤認する可能性があるため除外する。
fn parse_wxh(s: &str) -> Option<(u32, u32)> {
    let (w_str, h_str) = s.split_once('x')?;
    let w: u32 = w_str.parse().ok()?;
    let h: u32 = h_str.parse().ok()?;
    if w > 100 && h > 100 {
        Some((w, h))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(text: &str) -> Option<(u32, u32)> {
        parse_all_monitors(text).into_iter().next().map(|m| (m.width, m.height))
    }

    #[test]
    fn parses_inline_resolution() {
        let text = "Output: 1 DP-1 enabled connected primary geometry 0,0,2560x1440 resolution 2560x1440@60\n\
                    Output: 2 HDMI-1 disabled disconnected\n";
        assert_eq!(first(text), Some((2560, 1440)));
    }

    #[test]
    fn skips_disabled_output() {
        let text = "Output: 1 DP-1 disabled disconnected\n\
                    Output: 2 HDMI-2 enabled connected geometry 0,0,1920x1080 resolution 1920x1080@60\n";
        assert_eq!(first(text), Some((1920, 1080)));
    }

    #[test]
    fn parses_at_rate_notation() {
        let text = "Output: 1 eDP-1 enabled connected 1366x768@60\n";
        assert_eq!(first(text), Some((1366, 768)));
    }

    #[test]
    fn parses_current_mode_in_modes_section() {
        let text = "Output: 1 eDP-1 enabled connected primary\n\
                    \tmodes:\n\
                    \t  1: 1920x1080@60 *current\n\
                    \t  2: 1280x720@60\n";
        assert_eq!(first(text), Some((1920, 1080)));
    }

    #[test]
    fn returns_none_for_empty() {
        assert_eq!(first(""), None);
    }

    #[test]
    fn returns_none_when_all_disabled() {
        let text = "Output: 1 DP-1 disabled disconnected\n\
                    Output: 2 HDMI-1 disabled disconnected\n";
        assert_eq!(first(text), None);
    }

    #[test]
    fn current_mode_overrides_geometry_line() {
        let text = "Output: 1 DP-1 enabled connected geometry 0,0,2560x1440\n\
                    \tmodes:\n\
                    \t  1: 2560x1440@60\n\
                    \t  2: 1920x1080@60 *current\n";
        assert_eq!(first(text), Some((1920, 1080)));
    }

    #[test]
    fn ignores_small_coordinate_values() {
        let text = "Output: 1 DP-1 enabled connected geometry 0,0,3840x2160@30\n";
        assert_eq!(first(text), Some((3840, 2160)));
    }

    #[test]
    fn parse_all_monitors_two_enabled() {
        let text = "Output: 1 DP-1 enabled connected primary geometry 0,0,2560x1440 resolution 2560x1440@60\n\
                    Output: 2 HDMI-1 enabled connected geometry 2560,0,1920x1080 resolution 1920x1080@60\n";
        let monitors = parse_all_monitors(text);
        assert_eq!(monitors.len(), 2);
        assert_eq!(monitors[0].name, "DP-1");
        assert_eq!(monitors[0].width, 2560);
        assert_eq!(monitors[0].height, 1440);
        assert_eq!(monitors[1].name, "HDMI-1");
        assert_eq!(monitors[1].width, 1920);
        assert_eq!(monitors[1].height, 1080);
    }

    #[test]
    fn parse_all_monitors_skips_disabled() {
        let text = "Output: 1 DP-1 enabled connected primary geometry 0,0,2560x1440\n\
                    Output: 2 HDMI-1 disabled disconnected\n";
        let monitors = parse_all_monitors(text);
        assert_eq!(monitors.len(), 1);
        assert_eq!(monitors[0].name, "DP-1");
    }

    #[test]
    fn parse_all_monitors_current_mode() {
        let text = "Output: 1 eDP-1 enabled connected primary\n\
                    \tmodes:\n\
                    \t  1: 1920x1080@60 *current\n\
                    \t  2: 1280x720@60\n";
        let monitors = parse_all_monitors(text);
        assert_eq!(monitors.len(), 1);
        assert_eq!(monitors[0].width, 1920);
        assert_eq!(monitors[0].height, 1080);
    }

    #[test]
    fn parse_all_monitors_empty() {
        assert!(parse_all_monitors("").is_empty());
    }
}
