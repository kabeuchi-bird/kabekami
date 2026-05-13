//! モニター構成の動的更新ウォッチャー。
//!
//! 起動時の `resolve_screens()` が失敗（kscreen-doctor 未起動等）してフォールバック
//! 解像度に落ちた場合でも、Plasma が後から立ち上がってきた時点で正しい解像度を
//! 取得できるよう、定期的に `screen::detect_all()` を呼んで差分を検出する。
//!
//! 副次効果としてモニターのホットプラグ／解像度変更にも追従できる。
//!
//! ## 設計
//! - インターバルは 60 秒（kscreen-doctor 1 プロセスぶんなので十分軽量）
//! - 環境変数 `KABEKAMI_SCREEN` が設定されている場合は監視しない（ユーザー上書きを尊重）
//! - 差分が検出されたら `TrayCmd::ScreensChanged` を送信し、メインループ側で
//!   `screens` を差し替えて壁紙を再適用する。
//!
//! ## なぜポーリング？
//! KDE には残念ながら全バージョンで安定した「画面構成変更シグナル」の D-Bus 公開が
//! ないため、kscreen-doctor を周期実行する単純な方法に倒している。

use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use crate::screen::{self, Monitor};
use crate::tray::TrayCmd;

const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// モニター構成の変化を周期的に検出するタスクをバックグラウンドで起動する。
///
/// `KABEKAMI_SCREEN` が設定されている場合は何もせずに戻る。
pub fn spawn(initial: Vec<Monitor>, tx: UnboundedSender<TrayCmd>) {
    if std::env::var_os("KABEKAMI_SCREEN").is_some() {
        tracing::debug!("screen watcher: KABEKAMI_SCREEN is set, watcher disabled");
        return;
    }

    tokio::spawn(async move {
        let mut current = initial;
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // 初回 tick は即時なので捨てる（起動時の resolve_screens と重複させない）
        ticker.tick().await;

        loop {
            ticker.tick().await;
            let detected = tokio::task::spawn_blocking(screen::detect_all)
                .await
                .unwrap_or_default();
            // 0 件は「kscreen-doctor が一時的に応答していない」ケースとして無視する。
            // 既知の構成を壊さないよう、明示的に >0 件のときだけ比較する。
            if detected.is_empty() {
                continue;
            }
            if detected != current {
                tracing::info!(
                    "screens changed: {} → {} monitor(s)",
                    current.len(),
                    detected.len()
                );
                current = detected.clone();
                let _ = tx.send(TrayCmd::ScreensChanged(detected));
            }
        }
    });
}
