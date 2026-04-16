//! 画面解像度の自動取得。設計書 §7 に準拠。
//!
//! `kscreen-doctor --outputs` の出力をパースして、最初の `enabled` な
//! 出力（モニター）の解像度を返す。環境変数 `KABEKAMI_SCREEN=WxH` が
//! 設定されている場合は main.rs 側で優先して使用され、この関数は呼ばれない。
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

/// 画面解像度を自動検出する。
///
/// `kscreen-doctor --outputs` を実行し、最初の `enabled` な出力の
/// 解像度を返す。コマンドが見つからない / 失敗した場合は `None`。
pub fn detect() -> Option<(u32, u32)> {
    let output = std::process::Command::new("kscreen-doctor")
        .arg("--outputs")
        .output()
        .ok()?;

    if !output.status.success() {
        tracing::warn!("kscreen-doctor exited with non-zero status");
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    tracing::debug!("kscreen-doctor output:\n{}", stdout);
    parse_primary_resolution(&stdout)
}

/// テキストから最初の enabled 出力の解像度を取り出す。
///
/// 出力行のトークンを直接スキャンするため、"Output:" 行のインライン記述と
/// 後続行（modes: セクション）の両方に対応する。
///
/// `*current` マーカー付きの行（現在のアクティブモード）を最優先とし、
/// それが存在しない場合はブロック内で最初に見つかった WxH を使う。
fn parse_primary_resolution(text: &str) -> Option<(u32, u32)> {
    let mut in_enabled = false;
    // enabled ブロック内で最初に見つかった候補（geometry 等）
    let mut candidate: Option<(u32, u32)> = None;

    for line in text.lines() {
        let trimmed = line.trim();

        // 新しい Output ブロックの開始
        if trimmed.starts_with("Output:") {
            // 前の enabled ブロックに *current がなかった場合は candidate を返す
            if in_enabled {
                if let Some(res) = candidate {
                    return Some(res);
                }
            }
            in_enabled = trimmed.contains("enabled");
            candidate = None;
        }

        if !in_enabled {
            continue;
        }

        // 行内の各トークンから "WxH" / "WxH@rate" パターンを探す。
        // カンマ区切りの座標 "0,0,2560x1440" も分解できるよう
        // ホワイトスペースとカンマの両方で分割する。
        for token in trimmed.split(|c: char| c.is_ascii_whitespace() || c == ',') {
            let base = token.split('@').next().unwrap_or(token);
            if let Some(res) = parse_wxh(base) {
                if trimmed.contains("*current") {
                    // *current 行は最も信頼性が高いので即確定
                    return Some(res);
                }
                // それ以外はブロック内の最初の候補として記録しておく
                candidate.get_or_insert(res);
                break;
            }
        }
    }

    // 最後の enabled ブロックの候補を返す
    if in_enabled { candidate } else { None }
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

    #[test]
    fn parses_inline_resolution() {
        let text = "Output: 1 DP-1 enabled connected primary geometry 0,0,2560x1440 resolution 2560x1440@60\n\
                    Output: 2 HDMI-1 disabled disconnected\n";
        assert_eq!(parse_primary_resolution(text), Some((2560, 1440)));
    }

    #[test]
    fn skips_disabled_output() {
        let text = "Output: 1 DP-1 disabled disconnected\n\
                    Output: 2 HDMI-2 enabled connected geometry 0,0,1920x1080 resolution 1920x1080@60\n";
        assert_eq!(parse_primary_resolution(text), Some((1920, 1080)));
    }

    #[test]
    fn parses_at_rate_notation() {
        let text = "Output: 1 eDP-1 enabled connected 1366x768@60\n";
        assert_eq!(parse_primary_resolution(text), Some((1366, 768)));
    }

    #[test]
    fn parses_current_mode_in_modes_section() {
        let text = "Output: 1 eDP-1 enabled connected primary\n\
                    \tmodes:\n\
                    \t  1: 1920x1080@60 *current\n\
                    \t  2: 1280x720@60\n";
        assert_eq!(parse_primary_resolution(text), Some((1920, 1080)));
    }

    #[test]
    fn returns_none_for_empty() {
        assert_eq!(parse_primary_resolution(""), None);
    }

    #[test]
    fn returns_none_when_all_disabled() {
        let text = "Output: 1 DP-1 disabled disconnected\n\
                    Output: 2 HDMI-1 disabled disconnected\n";
        assert_eq!(parse_primary_resolution(text), None);
    }

    #[test]
    fn current_mode_overrides_geometry_line() {
        // geometry says 2560x1440 but the active mode (*current) is 1920x1080
        let text = "Output: 1 DP-1 enabled connected geometry 0,0,2560x1440\n\
                    \tmodes:\n\
                    \t  1: 2560x1440@60\n\
                    \t  2: 1920x1080@60 *current\n";
        assert_eq!(parse_primary_resolution(text), Some((1920, 1080)));
    }

    #[test]
    fn ignores_small_coordinate_values() {
        // geometry row offset "0,0,..." should not match the 0,0 part
        let text = "Output: 1 DP-1 enabled connected geometry 0,0,3840x2160@30\n";
        // 0x0 would fail w>100 check; 3840x2160 passes
        assert_eq!(parse_primary_resolution(text), Some((3840, 2160)));
    }
}
