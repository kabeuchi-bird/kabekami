//! モニター構成の動的更新ウォッチャー。
//!
//! 起動時の `resolve_screens()` が失敗（kscreen-doctor 未起動等）してフォールバック
//! 解像度に落ちた場合でも、Plasma が後から立ち上がってきた時点で正しい解像度を
//! 取得できるよう、壁紙更新のタイミングで `screen::detect_all()` を呼び直して
//! 差分を検出する。副次効果としてモニターのホットプラグ・解像度変更にも追従できる。
//!
//! ## 設計
//! - **トリガー**: 壁紙更新イベント（`trigger_tx.send(())` 経由）
//! - **スロットル**: 連続呼び出しは最低 60 秒間隔まで間引く。短い rotation
//!   interval （例: 10 秒）でも kscreen-doctor を叩きすぎないようにする。
//! - 環境変数 `KABEKAMI_SCREEN` が設定されている場合は無効化（ユーザー上書きを尊重）
//! - 差分検出時は `TrayCmd::ScreensChanged` を送信し、メインループ側で
//!   `screens` を差し替えて壁紙を再適用する。
//!
//! ## なぜシグナル監視ではなくポーリング？
//! KDE には全バージョンで安定した「画面構成変更」D-Bus シグナルが無いため、
//! 壁紙更新のタイミングで kscreen-doctor を呼び直すという単純な方法に倒している。

use std::time::{Duration, Instant};

use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use crate::screen::{self, Monitor};
use crate::tray::TrayCmd;

/// 連続検出の最小間隔。短い rotation interval での kscreen-doctor 連打を防ぐ。
const MIN_INTERVAL: Duration = Duration::from_secs(60);

/// 画面構成検出ウォッチャーをバックグラウンドで起動する。
///
/// - `Some(trigger_tx)` を返した場合、`trigger_tx.send(())` で検出を要求できる。
///   メインループは壁紙更新の直後にこれを呼ぶ。
/// - `KABEKAMI_SCREEN` が設定されている場合は `None` を返す（動的検出を行わない）。
pub fn spawn(
    initial: Vec<Monitor>,
    cmd_tx: UnboundedSender<TrayCmd>,
) -> Option<UnboundedSender<()>> {
    if std::env::var_os("KABEKAMI_SCREEN").is_some() {
        tracing::debug!("screen watcher: KABEKAMI_SCREEN is set, watcher disabled");
        return None;
    }

    let (trigger_tx, mut trigger_rx) = unbounded_channel::<()>();

    tokio::spawn(async move {
        let mut current = initial;
        let mut last_detect: Option<Instant> = None;

        while trigger_rx.recv().await.is_some() {
            // バックログをドレイン（連続トリガーは 1 回にまとめる）
            while trigger_rx.try_recv().is_ok() {}

            // スロットル: 前回検出から MIN_INTERVAL 経過していなければスキップ
            if let Some(t) = last_detect {
                if t.elapsed() < MIN_INTERVAL {
                    continue;
                }
            }

            let detected = tokio::task::spawn_blocking(screen::detect_all)
                .await
                .unwrap_or_else(|e| {
                    tracing::error!("screen detection task panicked: {}", e);
                    Vec::new()
                });

            // 0 件は「kscreen-doctor が一時的に応答していない」ケースとして無視
            // （既知構成を壊さないため送信はスキップするが、throttle は適用する）
            if detected.is_empty() {
                last_detect = Some(Instant::now());
                continue;
            }

            last_detect = Some(Instant::now());

            if detected != current {
                tracing::info!(
                    "screens changed: {} → {} monitor(s)",
                    current.len(),
                    detected.len()
                );
                current = detected.clone();
                if let Err(_) = cmd_tx.send(TrayCmd::ScreensChanged(detected)) {
                    tracing::debug!("screen watcher: receiver closed, exiting");
                    break;
                }
            }
        }
    });

    Some(trigger_tx)
}
