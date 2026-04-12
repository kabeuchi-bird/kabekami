# kabekami（壁紙） — 設計ドキュメント

KDE Plasma 向け壁紙ローテーションツール。Rust 実装。

---

## 1. スコープ

- ローカル画像のローテーション（順次 / ランダム）
- KDE Plasma 専用（Plasma 5.7+ / Plasma 6）
- Variety 互換の表示モード（blur-pad を含む）
- システムトレイ常駐＋コンテキストメニュー操作

スコープ外: 引用文オーバーレイ、時計オーバーレイ、マルチモニタ個別制御（将来課題）

将来対応: オンライン壁紙ソースからの自動取得（→ §16）

---

## 2. アーキテクチャ概要

```
┌────────────────────────────────────────────────────┐
│                     kabekami                        │
│                                                    │
│  ┌──────────┐  ┌───────────┐  ┌───────────┐       │
│  │ Scheduler │─▶│ ImageProc │─▶│ PlasmaAPI │       │
│  │ (タイマー) │  │ (加工)    │  │ (壁紙設定)│       │
│  └──────────┘  └───────────┘  └───────────┘       │
│       │  ▲          ▲                              │
│       │  │     ┌────┴────┐   ┌───────────┐         │
│       │  │     │ Prefetch│   │  Cache    │         │
│       │  │     │ (先読み) │──▶│ (LRU)    │         │
│       │  │     └─────────┘   └───────────┘         │
│       ▼  │                                         │
│  ┌──────────┐  ┌───────────┐                       │
│  │ TrayIcon │  │  Config   │                       │
│  │ (ksni)   │  │  (TOML)   │                       │
│  └──────────┘  └───────────┘                       │
└────────────────────────────────────────────────────┘
```

デーモンプロセスとして常駐。Qt は使わず、トレイアイコンは `ksni` クレートで
StatusNotifierItem (SNI) プロトコル経由にする。KDE Plasma は SNI をネイティブ
サポートしているため、cxx-qt で QSystemTrayIcon をブリッジするよりも
大幅に軽量かつ安定する。

---

## 3. 主要クレート

| 用途 | クレート | 備考 |
|---|---|---|
| システムトレイ | `ksni` (≥0.3, blocking feature) | SNI プロトコル。KDE ネイティブ |
| D-Bus | `zbus` (≥5) | Plasma evaluateScript 呼び出し |
| 画像処理 | `image` (≥0.25) | resize, blur, composite |
| 設定 | `serde` + `toml` | TOML パース |
| ランダム | `rand` | シャッフル |
| ファイル監視 | `notify` (≥7) | ディレクトリ変更検知（任意） |
| ログ | `tracing` | デバッグ用 |
| 非同期ランタイム | `tokio` | ksni デフォルト + zbus 用 |

### cxx-qt を使わない理由

このプロジェクトの UI はトレイアイコン＋コンテキストメニューのみ。
cxx-qt は Qt のフルバインディングを引き込むため、ビルド時間・依存関係・
CMake 統合の複雑さに対してメリットが薄い。将来、設定 GUI ウィンドウを
追加する場合は cxx-qt または egui を検討する。

---

## 4. 表示モード（DisplayMode）

Variety の表示モードに対応する。加工が必要なモードでは kabekami 側で
画像を生成し、KDE には常に「壁紙を画面に合わせて拡大」で渡す。

```rust
enum DisplayMode {
    /// 画面を埋める（はみ出し部分を切り取り）
    Fill,
    /// 画面に収める（余白は黒）
    Fit,
    /// アスペクト比無視で引き伸ばし
    Stretch,
    /// 画面に収め、余白を元画像のぼかしで埋める ★
    BlurPad,
    /// 画像サイズに応じて Fill/BlurPad を自動選択
    Smart,
}
```

### Smart モードのロジック

```
画像アスペクト比と画面アスペクト比の差が閾値(例: 0.15)以内
  → Fill（軽微なクロップで済む）
それ以外
  → BlurPad（大きなクロップになるため blur-pad が美しい）
```

---

## 5. BlurPad 画像処理パイプライン

これが最も重要な加工処理。`image` クレートだけで完結する。

```
入力: 元画像 (W_img × H_img), 画面解像度 (W_scr × H_scr)

1. 背景レイヤー生成
   a. 元画像を W_scr × H_scr にリサイズ（cover: アスペクト比維持で
      画面を完全に覆うサイズにリサイズ → 中央クロップ）
   b. ガウスぼかし適用（sigma = 20〜30 程度）
   c. 任意: 明度を少し落とす（前景を際立たせるため）

2. 前景レイヤー生成
   a. 元画像を W_scr × H_scr に収まるようリサイズ（contain:
      アスペクト比維持で最大辺が画面に一致）

3. 合成
   a. 背景レイヤーの中央に前景レイヤーをオーバーレイ

4. 出力
   a. キャッシュディレクトリに保存（JPEG, quality=92）
   b. この加工済みファイルのパスを Plasma に渡す
```

### 擬似コード

```rust
fn generate_blur_pad(
    src: &DynamicImage,
    screen_w: u32,
    screen_h: u32,
    blur_sigma: f32,
) -> RgbaImage {
    // 1. 背景: cover resize → crop → blur
    let bg_scale = f32::max(
        screen_w as f32 / src.width() as f32,
        screen_h as f32 / src.height() as f32,
    );
    let bg_resized = src.resize(
        (src.width() as f32 * bg_scale).ceil() as u32,
        (src.height() as f32 * bg_scale).ceil() as u32,
        FilterType::Triangle,
    );
    let bg_cropped = crop_center(&bg_resized, screen_w, screen_h);
    let bg_blurred = imageops::blur(&bg_cropped, blur_sigma);

    // 2. 前景: contain resize
    let fg = src.resize(screen_w, screen_h, FilterType::Lanczos3);

    // 3. 合成
    let offset_x = (screen_w - fg.width()) / 2;
    let offset_y = (screen_h - fg.height()) / 2;
    let mut canvas = bg_blurred;
    imageops::overlay(&mut canvas, &fg, offset_x as i64, offset_y as i64);

    canvas
}
```

### パフォーマンス考慮

`image` クレートの `blur()` は大きな sigma で遅くなる（O(sigma × pixels)）。
4K 画像で sigma=25 だと数秒かかる可能性がある。対策:

- 背景は低解像度でぼかしてから拡大する（見た目に差がない）
- 例: 1/4 サイズでぼかし → 画面サイズに拡大
- これで処理時間を 1/16 に短縮できる

#### 処理時間の見積もり（4K 画面の場合）

| 処理 | キャッシュなし | キャッシュあり |
|---|---|---|
| 画像読み込み＋デコード | ~100ms | ~100ms |
| BlurPad 加工（1/4 ぼかし最適化込み） | ~500–1500ms | スキップ |
| JPEG エンコード・書き出し | ~100–200ms | スキップ |
| Plasma 壁紙反映（D-Bus） | ~200–500ms | ~200–500ms |
| **合計** | **~1–2.5秒** | **~300–600ms** |

キャッシュヒット時は 10 秒間隔でも十分余裕がある。キャッシュミス時の
1–2.5 秒は短い間隔設定ではユーザー体験を損なう可能性があるため、
次節の先読み機構で解決する。

---

## 5a. 先読み（Prefetch）機構

短い切り替え間隔（10 秒等）でもスムーズに動作させるため、**次の壁紙を
バックグラウンドで事前加工する**仕組みを設ける。

### タイムライン

```
時刻 0s:    画像A を壁紙に設定
            └─ 即座にバックグラウンドで画像B の加工を開始（tokio::spawn）
時刻 ~1.5s: 画像B の加工完了 → キャッシュに保存
時刻 10s:   画像B を壁紙に設定（キャッシュヒット → 即座に反映）
            └─ 画像C の加工を開始
  ...繰り返し
```

壁紙設定のたびに「次に表示する画像」を確定させ、その加工を非同期で
走らせることで、切り替え時にはキャッシュが温まった状態にできる。

### 設計

```rust
/// 先読みキューの管理
struct Prefetcher {
    /// 先読み中のタスクハンドル（キャンセル用）
    pending: Option<JoinHandle<()>>,
}

impl Prefetcher {
    /// 壁紙設定の直後に呼ぶ。次の画像の加工をバックグラウンドで開始する。
    fn prefetch_next(
        &mut self,
        next_image: PathBuf,
        screen_size: (u32, u32),
        mode: DisplayMode,
        cache: Arc<Cache>,
    ) {
        // 前回の先読みがまだ走っていたらキャンセル
        // （ユーザーが「次へ」を連打した場合など）
        if let Some(handle) = self.pending.take() {
            handle.abort();
        }

        self.pending = Some(tokio::spawn(async move {
            if cache.has(&next_image, &mode, screen_size) {
                return; // すでにキャッシュにある
            }
            // blocking な画像加工は spawn_blocking で逃がす
            let result = tokio::task::spawn_blocking(move || {
                process_image(&next_image, screen_size, &mode)
            }).await;

            if let Ok(Ok(processed)) = result {
                cache.store(&next_image, &mode, screen_size, &processed);
            }
        }));
    }
}
```

### ランダムモードとの兼ね合い

ランダムモードでは「次に何を表示するか」が決まっていないと先読みできない。
そこで、画像リストを事前にシャッフルしてキュー化し、順に消費する方式にする:

```
起動時: [img3, img7, img1, img5, ...] ← Fisher-Yates シャッフル
         ^^^^  ^^^^
         現在   次（先読み対象）
```

リスト末尾に達したら再シャッフルする。これにより「ランダムだが
全画像を一巡する」動作になり、同じ画像が連続するのを防げる。

### 「次へ」「前へ」操作時の先読み

ユーザーがトレイメニューから「次の壁紙」を押した場合:

1. 先読み済みキャッシュがあれば即座に切り替え
2. 新たな「次の画像」の先読みを開始
3. 進行中だった先読みタスクは abort する

「前の壁紙」の場合は履歴スタックから取得するため先読みの必要はない
（すでに加工済みキャッシュが存在するはず）。

### Plasma 側の制約: 切り替え間隔の下限

Plasma は壁紙切り替え時に内部でフェードアニメーション（~300ms）を
行うことがある。極端に短い間隔（2–3 秒以下）ではアニメーション完了前に
次の設定が来てちらつく可能性がある。

設定ファイルの `interval_secs` に下限値（5 秒）を設ける:

```rust
const MIN_INTERVAL_SECS: u64 = 5;
```

### 方法: D-Bus evaluateScript

`zbus` で `org.kde.PlasmaShell` の `evaluateScript` メソッドを呼ぶ。

```rust
async fn set_wallpaper(path: &Path) -> zbus::Result<()> {
    let connection = zbus::Connection::session().await?;
    let path_str = path.canonicalize()?.display().to_string();

    let script = format!(r#"
        for (const desktop of desktops()) {{
            if (desktop.screen === -1) continue;
            desktop.wallpaperPlugin = "org.kde.image";
            desktop.currentConfigGroup = [
                "Wallpaper", "org.kde.image", "General"
            ];
            desktop.writeConfig("Image", "file://{}");
        }}
    "#, path_str);

    connection.call_method(
        Some("org.kde.plasmashell"),
        "/PlasmaShell",
        Some("org.kde.PlasmaShell"),
        "evaluateScript",
        &(script,),
    ).await?;

    Ok(())
}
```

### 注意点

- ウィジェットがロックされていると `evaluateScript` が失敗する
  （Plasma の制限。エラー時にユーザーへ通知する）
- BlurPad モードの場合、加工済み画像を渡すので KDE 側の FillMode は
  問わない（画像がすでに画面サイズぴったり）
- Fill / Fit / Stretch モードは KDE ネイティブの FillMode を使うことも
  可能だが、統一性のため全モードで kabekami 側で加工し、KDE には
  常に画面サイズの画像を渡す方がシンプル

### フォールバック

`evaluateScript` が使えない場合は `plasma-apply-wallpaperimage` CLI に
フォールバックする:

```rust
fn set_wallpaper_fallback(path: &Path) -> io::Result<()> {
    Command::new("plasma-apply-wallpaperimage")
        .arg(path)
        .status()?;
    Ok(())
}
```

---

## 7. 画面解像度の取得

壁紙加工に画面サイズが必要。KDE/Wayland 環境では:

```rust
// 方法1: kscreen-doctor（KDE 付属、Wayland 対応）
// `kscreen-doctor --outputs` の出力をパース
//   Output: 1 DP-1 enabled connected ... 2560x1440@144

// 方法2: wlr-randr（wlroots 系のみ）

// 方法3: D-Bus 経由で KScreen から取得
```

`kscreen-doctor` が最も安定。出力をパースして解像度を取る。
マルチモニタの場合はプライマリモニタの解像度を使う。

---

## 8. 設定ファイル

`~/.config/kabekami/config.toml`

```toml
[sources]
# 画像ディレクトリ（複数指定可）
directories = [
    "~/Pictures/Wallpapers",
    "~/Pictures/Photos",
]
# 再帰的にサブディレクトリを走査するか
recursive = true

[rotation]
# 切り替え間隔（秒）。下限 5 秒
interval_secs = 1800
# 順序: "sequential" | "random"
order = "random"
# 起動時に即座に壁紙を変更するか
change_on_start = true
# 次の壁紙を事前に加工しておく（短い間隔では必須）
prefetch = true

[display]
# モード: "fill" | "fit" | "stretch" | "blur_pad" | "smart"
mode = "blur_pad"
# BlurPad 用パラメータ
blur_sigma = 25.0
# 背景を暗くする量（0.0 = 変更なし, 0.3 = 30%暗く）
bg_darken = 0.1

[cache]
# 加工済み画像のキャッシュディレクトリ
directory = "~/.cache/kabekami"
# キャッシュの最大サイズ (MB)
max_size_mb = 500
```

---

## 9. モジュール構成

```
src/
├── main.rs          # エントリポイント、tokio ランタイム起動
├── config.rs        # TOML 設定の読み込み・デシリアライズ
├── scanner.rs       # 画像ファイルの走査・リスト構築
├── scheduler.rs     # タイマー管理、順次/ランダム選択
├── prefetch.rs      # 次画像の先読み（バックグラウンド加工）
├── display_mode.rs  # DisplayMode enum と画像加工ロジック
├── blur_pad.rs      # BlurPad 専用の画像処理パイプライン
├── plasma.rs        # KDE Plasma D-Bus / CLI 連携
├── screen.rs        # 画面解像度の取得
├── tray.rs          # ksni トレイアイコン・メニュー定義
└── cache.rs         # 加工済み画像のキャッシュ管理
```

---

## 10. トレイメニュー構成

```
kabekami
├── ▶ 次の壁紙
├── ◀ 前の壁紙
├── ─────────────
├── ⏸ 一時停止 / ▶ 再開
├── ─────────────
├── 表示モード ▶
│   ├── ○ Fill
│   ├── ○ Fit
│   ├── ○ Stretch
│   ├── ● BlurPad        ← ラジオボタン
│   └── ○ Smart
├── 切り替え間隔 ▶
│   ├── ○ 10秒
│   ├── ○ 30秒
│   ├── ○ 5分
│   ├── ● 30分
│   ├── ○ 1時間
│   └── ○ 3時間
├── ─────────────
├── 現在の壁紙を開く
├── ─────────────
└── 終了
```

`ksni` の `RadioGroup` と `SubMenu` でこの構造を実現できる。

---

## 11. 処理フロー

```
起動
 │
 ├─ config.toml を読み込み（interval_secs < 5 なら 5 に補正）
 ├─ 画像ディレクトリをスキャンしてリスト構築
 │   └─ random モードなら Fisher-Yates シャッフルでキュー化
 ├─ ksni トレイアイコンを起動
 │
 ├─ change_on_start == true なら即座に壁紙設定
 │   │
 │   ├─ キューから画像を選択（random or sequential）
 │   ├─ DisplayMode に応じて画像を加工
 │   │   ├─ キャッシュにヒットすれば再利用
 │   │   └─ なければ生成してキャッシュに保存
 │   ├─ plasma::set_wallpaper() で設定
 │   └─ ★ prefetch: 次の画像の加工をバックグラウンドで開始
 │
 └─ タイマーループ開始
     │
     └─ interval_secs ごとに:
         ├─ 次の画像を壁紙に設定（先読み済みならキャッシュヒット）
         └─ ★ prefetch: さらに次の画像の加工をバックグラウンドで開始
         （トレイメニューの「次/前」操作でも同じフローが発火。
          「次へ」連打時は進行中の先読みタスクを abort して再開）
```

---

## 12. キャッシュ戦略

加工済み画像のキャッシュキーは以下のハッシュ:

```
SHA256(元画像パス + 画面解像度 + DisplayMode + blur_sigma + bg_darken)
```

- キャッシュヒット → 画像加工をスキップし即座に壁紙設定
- キャッシュミス → 加工して保存（JPEG, quality=92）
- `max_size_mb` を超えたら古いキャッシュから削除（LRU）

---

## 13. 実装の優先順位

### Phase 1: 最小動作（MVP）

1. `config.rs` — TOML 読み込み
2. `scanner.rs` — ディレクトリ走査
3. `blur_pad.rs` — BlurPad 画像加工
4. `plasma.rs` — `plasma-apply-wallpaperimage` で壁紙設定
5. `main.rs` — タイマーループで定期切り替え（トレイなし、CLI のみ）

この時点でコア機能が動作確認できる。

### Phase 2: 短間隔対応＋トレイ

6. `cache.rs` — 加工済み画像のキャッシュ管理
7. `prefetch.rs` — 次画像の先読み（バックグラウンド加工）
8. `tray.rs` — ksni でトレイアイコン・メニュー
9. `scheduler.rs` — 前/次、一時停止/再開、シャッフルキュー

Phase 2 完了で 10 秒間隔が安定動作する。

### Phase 3: 磨き込み

10. `screen.rs` — 画面解像度の自動取得
11. `plasma.rs` — D-Bus evaluateScript 対応
12. `display_mode.rs` — Fill / Fit / Stretch / Smart の実装
13. `notify` によるディレクトリ監視（画像追加/削除の自動反映）

---

## 14. ビルドと配布

```toml
# Cargo.toml
[package]
name = "kabekami"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
ksni = "0.3"
zbus = "5"
image = "0.25"
serde = { version = "1", features = ["derive"] }
toml = "0.8"
rand = "0.9"
tracing = "0.1"
tracing-subscriber = "0.3"
sha2 = "0.10"
dirs = "6"
```

システム依存: なし（pure Rust）。`cxx-qt` / Qt / CMake 不要。
`cargo build --release` 一発でシングルバイナリが生成される。

---

## 15. README に含める内容

README.md には最低限以下を記載する:

- プロジェクト概要（KDE Plasma 向け壁紙ローテーションツール）
- スクリーンショット（BlurPad の動作例）
- インストール方法（`cargo install` / AUR 等）
- 設定ファイルの書き方
- 使い方（CLI オプション、トレイメニュー）
- ライセンス

### Acknowledgments セクション

README の末尾に以下の趣旨の謝辞を必ず入れる:

> **Acknowledgments**
>
> kabekami は [Variety](https://github.com/varietywalls/variety) に強く
> インスパイアされたプロジェクトです。Variety の作者である
> **Peter Levi** 氏、および長年にわたりメンテナンスを続けてきた
> コントリビューターの皆さんに深く感謝します。
> Variety が培ってきた壁紙表示モードの設計思想（特に blur-pad や
> smart fit）は、kabekami の中核的な機能の着想源となっています。

コードの直接的な流用はないが、機能設計・表示モードの分類・
set_wallpaper スクリプトの DE 判定ロジック等、Variety のアプローチを
参考にしている箇所が多いため、敬意を明示する。

---

## 16. 将来対応: オンライン壁紙ソース

### 概要

Bing Daily Image、Unsplash、Wallhaven 等のオンラインサービスから壁紙を
自動取得する機能を将来的に追加する。ローカル画像と同じローテーション
パイプラインに乗せるため、**Provider トレイト**による抽象化を設計時点
から意識しておく。

### Provider トレイト設計

```rust
/// 壁紙の取得元を抽象化するトレイト
#[async_trait]
trait WallpaperProvider: Send + Sync {
    /// プロバイダの識別名（"bing", "unsplash", "wallhaven", "local" 等）
    fn name(&self) -> &str;

    /// 新しい壁紙を取得してローカルにダウンロード。
    /// 返値はダウンロードした画像ファイルのパス。
    async fn fetch(&self, download_dir: &Path) -> Result<Vec<PathBuf>>;

    /// このプロバイダが API キーを必要とするか
    fn requires_api_key(&self) -> bool { false }
}
```

ローカルディレクトリも `WallpaperProvider` として実装する
（`fetch()` は単にディレクトリ走査を行いパスを返す）。これにより
Scheduler はソースの種類を意識せず統一的に扱える。

### 想定するプロバイダ

| プロバイダ | API キー | 取得方式 | 備考 |
|---|---|---|---|
| `local` | 不要 | ディレクトリ走査 | 既存実装をラップ |
| `bing` | 不要 | JSON API | 日替わり 1 枚。`https://www.bing.com/HPImageArchive.aspx?format=js&n=8` |
| `unsplash` | 必要 | REST API | `https://api.unsplash.com/photos/random`。無料枠 50 req/hr |
| `wallhaven` | 任意 | REST API | API キーがあれば NSFW 対応。ページネーションあり |
| `reddit` | 不要 | JSON (.json 付加) | `/r/wallpapers/hot.json` 等。直リンク画像のみフィルタ |

### 設定ファイル拡張案

```toml
# 既存のローカルソース
[[sources]]
type = "local"
directories = ["~/Pictures/Wallpapers"]
recursive = true

# Bing 日替わり壁紙
[[sources]]
type = "bing"
market = "ja-JP"        # 地域（任意）
count = 8               # 過去何日分を取得するか（最大 8）

# Unsplash ランダム
[[sources]]
type = "unsplash"
api_key = "YOUR_ACCESS_KEY"
query = "nature,landscape"  # 検索キーワード（任意）
orientation = "landscape"
count = 10                  # 1 回の取得枚数

# Wallhaven
[[sources]]
type = "wallhaven"
api_key = ""                # 空なら SFW のみ
query = "landscape"
categories = "general"      # general / anime / people
purity = "sfw"              # sfw / sketchy / nsfw（API キー必須）
count = 20

# Reddit
[[sources]]
type = "reddit"
subreddits = ["wallpapers", "wallpaper", "EarthPorn"]
sort = "hot"                # hot / new / top
count = 20
min_width = 1920            # 小さい画像を除外
min_height = 1080
```

`[sources]` を `[[sources]]` の配列に変更し、複数のソースを混在させる。
現行の `[sources]` 形式からのマイグレーション処理も必要。

### ダウンロード管理

```
~/.cache/kabekami/
├── processed/        # 加工済み画像（既存のキャッシュ）
└── downloads/        # オンラインから取得した元画像
    ├── bing/
    ├── unsplash/
    ├── wallhaven/
    └── reddit/
```

- ダウンロードした画像はプロバイダごとのサブディレクトリに保存
- 重複ダウンロード防止: ファイル名 or URL ベースのチェック
- 容量制限: `max_download_mb` を設定可能に（古い画像から削除）
- ネットワーク不通時: ダウンロード済み画像 + ローカル画像で継続

### フェッチスケジューリング

壁紙切り替えのたびに API を叩くのではなく、バッチ取得する:

```
起動時 / 一定間隔（例: 6 時間ごと）:
  各 Provider の fetch() を実行 → downloads/ に保存
  ↓
Scheduler のローテーションキューにダウンロード済み画像を追加
  ↓
通常の切り替えループで消費
```

これにより API のレートリミットに抵触しにくく、オフライン時にも
ダウンロード済み画像を使い続けられる。

### モジュール構成への追加

```
src/
├── provider/
│   ├── mod.rs          # WallpaperProvider トレイト定義
│   ├── local.rs        # ローカルディレクトリ
│   ├── bing.rs         # Bing Daily Image
│   ├── unsplash.rs     # Unsplash API
│   ├── wallhaven.rs    # Wallhaven API
│   └── reddit.rs       # Reddit JSON
├── download.rs         # ダウンロード管理・重複排除・容量制限
...
```

### 追加クレート

| 用途 | クレート |
|---|---|
| HTTP クライアント | `reqwest` (async, TLS) |
| JSON パース | `serde_json` |

`reqwest` は既に tokio ベースなので、現行アーキテクチャにそのまま
組み込める。

### 実装の優先順位（Phase 4 として）

1. `WallpaperProvider` トレイトと `local` プロバイダへのリファクタリング
2. `bing` プロバイダ（API キー不要で最もシンプル）
3. ダウンロード管理（重複排除・容量制限）
4. `unsplash` プロバイダ
5. `wallhaven` プロバイダ
6. `reddit` プロバイダ

---

## 17. 将来対応: GUI 設定画面

### 方針: デーモンと GUI を分離

本体デーモン（`kabekami`）と設定 GUI（`kabekami-config`）を**別バイナリ**
にする。理由:

- デーモン側の ksni + tokio イベントループと、GUI の eframe イベント
  ループを同一プロセスに同居させるとライフタイム管理が複雑になる
- GUI は TOML の読み書きだけなので、デーモンの設計を一切変えずに済む
- GUI を使わず手動で TOML を編集するパワーユーザーにも対応できる
- デーモンが常時メモリに載るのに対し、GUI は設定時だけ起動すればよい

### GUI ツールキットの選択: egui (eframe)

| 選択肢 | メリット | デメリット |
|---|---|---|
| **egui (eframe)** ★ | cargo のみでビルド、tokio 不要、クロスプラットフォーム | KDE ネイティブの外観にならない |
| cxx-qt (Qt/QML) | KDE ネイティブ外観 | CMake 統合が重い、ビルド複雑、設定画面だけには過剰 |
| GTK4 (gtk4-rs) | GNOME 系なら自然 | KDE 環境では浮く、libgtk4 が必要 |
| Web UI (localhost) | 実装が手軽 | ブラウザ依存、UX が異質 |

egui を選択する。設定画面程度のフォーム UI であれば egui の即時モード
レンダリングで十分実用的。

### デーモンとの通信

```
kabekami-config                    kabekami (デーモン)
┌─────────────┐                   ┌─────────────┐
│  TOML 読込  │                   │             │
│     ↓       │                   │  TOML 監視  │
│  GUI 編集   │                   │  (notify)   │
│     ↓       │                   │     or      │
│  TOML 書出  │──── config.toml ──▶│  D-Bus      │
│     ↓       │                   │  シグナル   │
│  リロード   │── SIGUSR1 / D-Bus ▶│  → 再読込   │
│  通知送信   │                   │             │
└─────────────┘                   └─────────────┘
```

通信方式は以下の 3 案から選べる（併用も可）:

1. **TOML ファイル監視** — デーモン側で `notify` クレートにより
   `config.toml` の変更を検知して自動リロード。最もシンプル
2. **SIGUSR1** — GUI が保存後に `pkill -USR1 kabekami` で通知。
   Unix 的だが Windows 非対応
3. **D-Bus メソッド** — デーモンが `org.kabekami.Daemon.Reload` のような
   メソッドを公開。最もきちんとした方式で、トレイメニューから
   「設定を開く」→ GUI 起動 → 保存 → リロード の導線もきれいになる

Phase 5 の初期段階では方式 1（ファイル監視）で十分。

### GUI 画面構成

```
┌─ kabekami 設定 ──────────────────────────────────┐
│                                                   │
│  ┌─ 画像ソース ─────────────────────────────────┐ │
│  │ [~/Pictures/Wallpapers]           [削除] [追加]│ │
│  │ [~/Photos/Nature]                 [削除]      │ │
│  │ ☑ サブディレクトリも含める                     │ │
│  └───────────────────────────────────────────────┘ │
│                                                   │
│  ┌─ ローテーション ─────────────────────────────┐ │
│  │ 切り替え間隔: [====●============] 30分        │ │
│  │ 順序:  ○ 順次  ● ランダム                     │ │
│  │ ☑ 起動時に壁紙を変更                          │ │
│  │ ☑ 次の壁紙を先読みする                        │ │
│  └───────────────────────────────────────────────┘ │
│                                                   │
│  ┌─ 表示モード ─────────────────────────────────┐ │
│  │ ○ Fill  ○ Fit  ○ Stretch  ● BlurPad  ○ Smart │ │
│  │                                               │ │
│  │ ぼかし強度: [========●======] 25.0             │ │
│  │ 背景暗化:   [==●============]  0.1             │ │
│  │                                               │ │
│  │ ┌─ プレビュー ──────────────────────────────┐ │ │
│  │ │                                           │ │ │
│  │ │     (現在の壁紙の加工プレビュー)           │ │ │
│  │ │                                           │ │ │
│  │ └───────────────────────────────────────────┘ │ │
│  └───────────────────────────────────────────────┘ │
│                                                   │
│  ┌─ キャッシュ ─────────────────────────────────┐ │
│  │ キャッシュ使用量: 142 MB / 500 MB             │ │
│  │ [キャッシュをクリア]                           │ │
│  └───────────────────────────────────────────────┘ │
│                                                   │
│                         [キャンセル]  [保存して適用] │
└───────────────────────────────────────────────────┘
```

### プレビュー機能

BlurPad のパラメータ（blur_sigma, bg_darken）を変更したときに
リアルタイムでプレビューを見せる。egui の `TextureHandle` に
加工済み画像を描画する:

```rust
// スライダー変更時にプレビュー再生成（デバウンス付き）
if blur_sigma_changed || bg_darken_changed {
    // 低解像度（例: 640x360）でプレビュー生成して即座に表示
    let preview = generate_blur_pad(&sample_image, 640, 360, blur_sigma);
    self.preview_texture = Some(ctx.load_texture("preview", preview));
}
```

プレビューは低解像度で行うので遅延は感じない。

### トレイメニューからの起動

```
kabekami
├── ...
├── ─────────────
├── ⚙ 設定...          ← kabekami-config を子プロセスとして起動
├── ─────────────
└── 終了
```

```rust
// tray.rs 内
StandardItem {
    label: "設定...".into(),
    activate: Box::new(|_| {
        Command::new("kabekami-config").spawn().ok();
    }),
    ..Default::default()
}
```

### Cargo ワークスペース構成

GUI を追加する段階で、プロジェクトをワークスペースに再編する:

```
kabekami/
├── Cargo.toml              # [workspace]
├── crates/
│   ├── kabekami/            # デーモン本体
│   │   ├── Cargo.toml
│   │   └── src/
│   ├── kabekami-config/     # GUI 設定ツール
│   │   ├── Cargo.toml      # eframe, image, serde, toml
│   │   └── src/
│   └── kabekami-common/     # 共有型定義（Config, DisplayMode 等）
│       ├── Cargo.toml
│       └── src/
```

`kabekami-common` にデーモンと GUI の両方が使う型（`Config`,
`DisplayMode`, `RotationOrder` 等）を切り出す。TOML のシリアライズ/
デシリアライズ実装もここに置く。

### 追加クレート（kabekami-config 用）

| 用途 | クレート |
|---|---|
| GUI フレームワーク | `eframe` (≥0.31) |
| ファイルダイアログ | `rfd` |
| 画像テクスチャ | `egui_extras` (image feature) |

### 実装の優先順位（Phase 5 として）

1. ワークスペース再編 + `kabekami-common` の切り出し
2. 最小 GUI（ソースディレクトリ・間隔・表示モードの編集 + 保存）
3. デーモン側の設定リロード機構（`notify` によるファイル監視）
4. トレイメニューに「設定...」を追加
5. プレビュー機能
6. オンラインソース設定の GUI 対応（§16 と連動）



