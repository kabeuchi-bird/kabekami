//! 設定ファイルのリロード。
//!
//! `config.toml` の変更（自動検知・トレイ操作の保存後）を受けて、ソース一覧の再走査、
//! 監視の張り替え、キャッシュ／先読み／タイマー／言語の差し替え、現在の壁紙の
//! 再適用までを行う。メインループの状態を借用して書き換えるため、借りる先を
//! `ReloadCtx` にまとめて受け取る。

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Interval;

use kabekami_common::i18n::Lang;

use crate::blacklist::Blacklist;
use crate::cache::{Cache, CacheKey};
use crate::config::Config;
use crate::notify::Notifier;
use crate::prefetch::Prefetcher;
use crate::scheduler::Scheduler;
use crate::{
    apply_and_notify, make_ticker, plasma, screen, state, tray, update_tray, watcher, ApplyCtx,
};

/// リロードが読み書きするメインループの状態。
pub struct ReloadCtx<'a> {
    pub config: &'a mut Config,
    pub scheduler: &'a mut Scheduler,
    pub blacklist: &'a Blacklist,
    pub prefetcher: &'a mut Prefetcher,
    pub cache: &'a mut Arc<Cache>,
    pub ticker: &'a mut Interval,
    pub lang: &'a mut Lang,
    pub notifier: &'a mut Notifier,
    pub state_writer: &'a mut state::StateWriter,
    pub tray_handle: &'a Option<ksni::Handle<tray::KabekamiTray>>,
    pub plasma: &'a plasma::PlasmaShell,
    pub screens: &'a [screen::Monitor],
    pub screen_check_tx: Option<&'a UnboundedSender<()>>,
    /// 最後に成功して走査した対象。リロード時の再走査要否をこれと比べる。
    pub scanned_dirs: &'a mut Vec<PathBuf>,
    pub scanned_recursive: &'a mut bool,
    pub watch_rx: &'a mut tokio::sync::mpsc::Receiver<watcher::WatchEvent>,
    pub watcher_handle: &'a mut Option<watcher::DirWatcher>,
}

/// 先読みが温めるべきキー一式。先読みが OFF か次の画像が無ければ `None`。
///
/// `start_prefetch` と同じ材料（次の画像・画面・表示設定・ON/OFF）から求めるので、
/// 新旧で比べれば先読みのやり直し要否が分かる（条件を手で並べると書き漏れる）。
fn prefetch_target(
    scheduler: &Scheduler,
    screens: &[screen::Monitor],
    cfg: &Config,
) -> Option<Vec<CacheKey>> {
    if !cfg.rotation.prefetch {
        return None;
    }
    scheduler.peek_next().map(|next| crate::cache_keys(next, screens, cfg))
}

/// 設定リロードを丸ごと省いてよいか。
///
/// 内容が同一でも、前回の走査や監視登録が完遂していなければ省けない。走査が
/// 空・失敗した場合や監視登録が部分失敗した場合、設定保存が唯一の再試行契機に
/// なるので、内容が同一だからと弾くとその再試行が永久に走らない。
fn reload_is_a_noop(
    new_cfg: &Config,
    config: &Config,
    scanned_dirs: &[PathBuf],
    scanned_recursive: bool,
    watching_everything: bool,
) -> bool {
    new_cfg == config
        && crate::collect_source_dirs(new_cfg) == scanned_dirs
        && new_cfg.sources.recursive == scanned_recursive
        && watching_everything
}

/// 設定を読み直して反映する。
pub async fn reload_config(ctx: ReloadCtx<'_>) {
    let ReloadCtx {
        config,
        scheduler,
        blacklist,
        prefetcher,
        cache,
        ticker,
        lang,
        notifier,
        state_writer,
        tray_handle,
        plasma: plasma_shell,
        screens,
        screen_check_tx,
        scanned_dirs,
        scanned_recursive,
        watch_rx,
        watcher_handle,
    } = ctx;

    // `apply_and_notify` に渡す引数束。`config` などを書き換えたあとに組むので、
    // 使う場所で組み立てる。
    macro_rules! apply_ctx {
        () => {
            ApplyCtx {
                screens,
                config: &*config,
                cache: &*cache,
                plasma: plasma_shell,
                tray_handle,
                scheduler: &*scheduler,
                screen_check_tx,
                notifier: &mut *notifier,
                prefetcher: &mut *prefetcher,
                state_writer: &mut *state_writer,
            }
        };
    }

    match Config::load() {
        Err(e) => {
            tracing::error!(error = %e, "config reload failed");
            let msg = e.to_string();
            notifier.error(&msg, None).await;
            update_tray(tray_handle, move |t| t.last_error = Some(msg)).await;
        }
        // 内容が同一で、かつ前回の走査と監視登録がどちらも完遂して
        // いるときだけ何もしない。トレイからのモード／間隔変更で
        // デーモン自身が保存した場合もここで弾ける。
        //
        // 走査が空・失敗した場合や監視登録が部分失敗した場合は、
        // 設定保存が唯一の再試行契機になる。内容が同一だからと
        // ここで弾くと、その再試行が永久に走らない。
        Ok(new_cfg)
            if reload_is_a_noop(
                &new_cfg,
                config,
                scanned_dirs,
                *scanned_recursive,
                watcher_handle.as_ref().is_some_and(|w| w.is_complete()),
            ) =>
        {
            tracing::debug!("config unchanged, skipping reload");
        }
        Ok(new_cfg) => {
            tracing::info!("reloading config");

            // 対象ディレクトリが変わっていなければ再スキャンも監視の
            // 張り替えも不要。単一ワーカーなので、大きなソースでは
            // `interval_secs` を変えただけの保存でも数百 ms 止まりうる。
            let source_dirs = crate::collect_source_dirs(&new_cfg);
            let sources_changed = source_dirs != *scanned_dirs
                || new_cfg.sources.recursive != *scanned_recursive;
            // 監視が全ディレクトリに張れていなければ、設定保存が唯一の
            // 再スキャン契機になる。一部失敗も同じ扱い（そのディレクトリの
            // イベントは届かないので、再スキャンと登録再試行の両方が要る）。
            let watching_everything =
                watcher_handle.as_ref().is_some_and(|w| w.is_complete());
            let needs_rescan = sources_changed || !watching_everything;

            // 走行中の先読みが温めているキー一式（設定を差し替える前の値で求める）
            let warming = prefetch_target(scheduler, screens, config);
            let shown = scheduler.current().cloned();
            if needs_rescan {
                match crate::scan_images(&source_dirs, new_cfg.sources.recursive, blacklist).await {
                    Ok(images) if !images.is_empty() => {
                        tracing::info!("reload: {} image(s) found", images.len());
                        // 一時停止状態と現在画像は rebuild が引き継ぐ
                        scheduler.rebuild(images, new_cfg.rotation.order);
                        // 画像一覧を差し替えたときだけ監視対象も差し替える。
                        // 旧一覧を保ったまま監視だけ新ディレクトリへ移すと、
                        // 旧画像の削除イベントを取りこぼし、消えた画像が
                        // ローテーションに残り続ける。
                        (*watch_rx, *watcher_handle) = crate::spawn_dir_watcher_offloaded(
                            &source_dirs,
                            new_cfg.sources.recursive,
                        )
                        .await;
                        *scanned_dirs = source_dirs;
                        *scanned_recursive = new_cfg.sources.recursive;
                    }
                    // 空・失敗のときは画像一覧も監視対象も据え置く
                    // （ネットワークマウントの一時的な不在などで
                    // ローテーションが空になるのを避ける）。
                    // `scanned_dirs` を更新しないので次のリロードで再試行される。
                    Ok(_) => tracing::warn!(
                        "reload: no images found, keeping current list and watcher"
                    ),
                    Err(e) => tracing::warn!(
                        "reload: scan error, keeping current list and watcher: {}", e
                    ),
                }
            } else {
                tracing::debug!("reload: source dirs unchanged, skipping rescan");
            }

            // rebuild を通らなかった場合も並び順は反映する
            // （通っていれば同値なので何もしない）。
            scheduler.set_order(new_cfg.rotation.order);

            // キャッシュの置き場所が変わったか。容量はキーに効かないので見ない。
            // 変わっていなければ温まったキャッシュも走行中の先読みもそのまま活かす。
            let cache_changed = new_cfg.cache.directory != config.cache.directory;
            let display_changed = new_cfg.display != config.display;

            // 温めるべきキー一式が変わったときだけ捨てる（据え置きなら
            // ほぼ終わったデコードを捨てる理由がない）。キャッシュの置き場所は
            // キーに入らないが、`Cache` ごと差し替えるので別に見る。
            let prefetch_stale =
                cache_changed || prefetch_target(scheduler, screens, &new_cfg) != warming;
            if prefetch_stale {
                prefetcher.abort();
            }
            // 同じ置き場所なら作り直さない。作り直すと加工受付が空の別物になり、
            // 走行中の加工と同じキーを新旧の `Cache` が別々に加工してしまう。
            if cache_changed {
                *cache = Arc::new(Cache::new(
                    new_cfg.cache.directory.clone(),
                    new_cfg.cache.max_size_mb,
                ));
            } else {
                cache.set_max_size_mb(new_cfg.cache.max_size_mb);
            }

            // 間隔が変わったときだけ仕切り直す（無関係な項目の保存で切り替えを先送りしない）
            if new_cfg.rotation.interval_secs != config.rotation.interval_secs {
                *ticker = make_ticker(new_cfg.rotation.interval_secs);
            }

            let new_lang = crate::resolve_lang(&new_cfg);
            if new_lang != *lang {
                *lang = new_lang;
                *notifier = Notifier::new(*lang).await;
            }

            *config = new_cfg;

            // 見た目が変わりうるときだけ貼り直す（通知設定などの保存で
            // Plasma へ同じ壁紙を送り直さない）
            let reapply =
                display_changed || cache_changed || scheduler.current() != shown.as_ref();

            // rebuild を通れば新しい一覧に残っていた画像、通らなければ
            // 据え置きの current。どちらも「いま表示しているべき画像」。
            match scheduler.current().cloned() {
                // 記録は `apply_and_notify` 内、成功時のみ。先に persist
                // すると、適用に失敗した壁紙を「現在」として保存し、
                // 再起動後のトレイやゴミ箱操作が画面に無い画像を指す
                // （分岐を畳まないこと）。
                Some(cur) if reapply => {
                    apply_and_notify(&mut apply_ctx!(), &cur, "reload: reapply failed").await;
                }
                Some(_) => {}
                // current が落ちた場合は apply_and_notify を通らないので、
                // state に残る旧画像を明示的に消す。
                None => {
                    state_writer.persist(scheduler.is_paused(), None).await;
                }
            }
            // 貼り直した場合は `apply_and_notify` が先読みを掛け直す
            if !reapply && prefetch_stale {
                crate::start_prefetch(prefetcher, scheduler, screens, config, cache);
            }

            let mode = config.display.mode;
            let secs = config.rotation.interval_secs;
            let strings = crate::i18n::strings(*lang);
            let count = scheduler.image_count();
            let has_fav = config.sources.favorites_dir.is_some();
            let bl_enabled = config.ui.enable_blacklist;
            let name = crate::tray_display_name(scheduler.current().map(|p| p.as_path()));
            update_tray(tray_handle, move |t| {
                t.mode = mode;
                t.interval_secs = secs;
                t.strings = strings;
                t.image_count = count;
                t.has_favorites_dir = has_fav;
                t.blacklist_enabled = bl_enabled;
                t.current_name = name;
            })
            .await;

            tracing::info!("config reload complete");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内容が同一でも、走査や監視登録が完遂していなければリロードを省けない。
    /// 省いてしまうと、走査が空・失敗した状態や監視が部分失敗した状態から
    /// 永久に抜け出せなくなる（設定保存が唯一の再試行契機のため）。
    #[test]
    fn reload_is_only_a_noop_when_scan_and_watch_are_both_complete() {
        let mut config = Config::default();
        config.sources.directories = vec![std::path::PathBuf::from("/pics")];
        let scanned = vec![std::path::PathBuf::from("/pics")];
        let recursive = config.sources.recursive;

        assert!(
            reload_is_a_noop(&config, &config, &scanned, recursive, true),
            "同一内容 + 走査済み + 全監視済みなら省ける"
        );
        assert!(
            !reload_is_a_noop(&config, &config, &[], recursive, true),
            "走査が未完了なら省けない（空・失敗のあと再試行が要る）"
        );
        assert!(
            !reload_is_a_noop(&config, &config, &scanned, !recursive, true),
            "recursive が前回の走査と違えば省けない"
        );
        assert!(
            !reload_is_a_noop(&config, &config, &scanned, recursive, false),
            "監視が全ディレクトリに張れていなければ省けない"
        );

        let mut other = config.clone();
        other.rotation.interval_secs += 1;
        assert!(
            !reload_is_a_noop(&other, &config, &scanned, recursive, true),
            "内容が違えば当然省けない"
        );
    }

    /// 先読みのやり直し要否は新旧の `prefetch_target` の比較だけで決まる。
    /// 先読みに効く設定（ON/OFF・表示設定）はキーを変え、効かない設定は変えないこと。
    #[test]
    fn prefetch_target_tracks_only_what_the_prefetch_uses() {
        use crate::config::Order;
        let scheduler = Scheduler::new(vec![PathBuf::from("/pics/a.jpg")], Order::Sequential);
        let screens = [screen::Monitor { name: "DP-1".into(), width: 1920, height: 1080 }];
        let config = Config::default();
        let base = prefetch_target(&scheduler, &screens, &config);
        assert!(base.is_some(), "既定では次の画像を温める");

        let mut unrelated = config.clone();
        unrelated.rotation.interval_secs += 1;
        unrelated.ui.notify_fetch = !unrelated.ui.notify_fetch;
        assert_eq!(prefetch_target(&scheduler, &screens, &unrelated), base, "無関係な設定では変わらない");

        let mut off = config.clone();
        off.rotation.prefetch = false;
        assert_eq!(prefetch_target(&scheduler, &screens, &off), None, "OFF なら温めない");

        let mut display = config.clone();
        display.display.blur_sigma += 1.0;
        assert_ne!(prefetch_target(&scheduler, &screens, &display), base, "表示設定でキーが変わる");
    }
}
