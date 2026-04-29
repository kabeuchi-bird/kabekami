> [English README is here / 英語版はこちら](README.md)

# kabekami（壁紙）

KDE Plasma 向け壁紙ローテーションツール。Rust 製。

- ローカル画像をタイマーで順次 / ランダム切り替え
- **BlurPad** モード: 元画像をぼかした背景の中央に原画をオーバーレイ（[Variety](https://github.com/varietywalls/variety) の blur-pad に相当）
- システムトレイ常駐（SNI プロトコル）＋コンテキストメニュー操作
- 加工済み画像のキャッシュ（SHA256 キー、LRU 退避）で短い間隔でも高速
- 次の画像を事前にバックグラウンド加工しておく先読み機構
- **マルチモニター対応**: `kscreen-doctor` で全モニターを自動検出し、各画面の解像度に最適化した画像を個別に適用
- **オンライン壁紙ソース**: Bing Daily・Unsplash・Wallhaven・Reddit から指定間隔で自動ダウンロード
- **お気に入りフォルダ**: 現在の壁紙をワンクリックで指定フォルダにコピー
- **ゴミ箱に移動**: 現在の壁紙をシステムのゴミ箱に送り、次の壁紙へ自動遷移
- **セッション管理**: `logind` でグレースフルシャットダウン検知・Plasma 再起動時に壁紙を自動再適用
- **GUI 設定ツール**（`kabekami-config`）: egui 製の 6 タブ設定画面。BlurPad のリアルタイムプレビュー付き

## 動作要件

| 項目 | 要件 |
|---|---|
| OS | Linux |
| DE | KDE Plasma 5.7 以降 または Plasma 6 |
| Rust | 1.75 以降（edition 2021） |
| 外部コマンド | `plasma-apply-wallpaperimage`（Plasma 付属） |
| D-Bus | セッションバスへのアクセス（トレイ表示に必要） |
| `kscreen-doctor` | 任意 — マルチモニター自動検出に必要 |

> **Note** `plasma-apply-wallpaperimage` は KDE パッケージに同梱されています。
> Arch Linux では `plasma-workspace`、Fedora/Debian では `plasma-workspace` または
> `kde-plasma-desktop` に含まれています。
>
> `kscreen-doctor` は `kscreen` パッケージに含まれています。インストールされていない場合は
> 1920×1080、または `KABEKAMI_SCREEN` で指定した解像度にフォールバックします。

## インストール

### cargo build（推奨）

```bash
git clone https://github.com/kabeuchi-bird/kabekami.git
cd kabekami
cargo build --release
# 両方のバイナリをインストール
sudo install -m755 target/release/kabekami        /usr/local/bin/
sudo install -m755 target/release/kabekami-config /usr/local/bin/
```

### AUR（Arch Linux）

```bash
paru -S kabekami-git
# または
yay -S kabekami-git
```

## クイックスタート

1. **設定ファイルを作成する**

   ```bash
   mkdir -p ~/.config/kabekami
   ```

   `~/.config/kabekami/config.toml` を以下の内容で作成します:

   ```toml
   [sources]
   directories = ["~/Pictures/Wallpapers"]
   recursive = true

   [rotation]
   interval_secs = 1800   # 30 分ごとに切り替え
   order = "random"
   change_on_start = true

   [display]
   mode = "blur_pad"      # BlurPad モード（推奨）
   blur_sigma = 25.0
   bg_darken = 0.1

   [cache]
   directory = "~/.cache/kabekami"
   max_size_mb = 500

   [ui]
   language = "ja"        # "en"（英語）/ "ja"（日本語）/ "kansai"（関西弁）
   ```

   TOML を直接編集せずに設定したい場合は GUI 設定ツールをお使いください:

   ```bash
   kabekami-config
   ```

2. **起動する**

   ```bash
   kabekami
   ```

   起動するとシステムトレイにアイコンが表示されます。

3. **自動起動を設定する（任意）**

   以下のいずれかの方法で設定できます。

   **方法 A — KDE システム設定から設定する（推奨・GUI）**

   1. **システム設定** を開く
   2. 「**起動と終了**」→「**自動起動**」を選択
   3. 「**アプリケーションを追加...**」をクリック
   4. `kabekami` と入力して選択し、「OK」

   または `.desktop` ファイルを直接配置する方法もあります:

   ```bash
   cat > ~/.config/autostart/kabekami.desktop <<'EOF'
   [Desktop Entry]
   Name=kabekami
   GenericName=Wallpaper Rotator
   Comment=KDE Plasma wallpaper rotation tool
   Exec=kabekami
   Type=Application
   Categories=Utility;
   X-KDE-autostart-phase=2
   EOF
   ```

   > `X-KDE-autostart-phase=2` により、Plasma シェルの初期化が完了してから
   > 起動するためトレイアイコンが確実に表示されます。

   **方法 B — systemd ユーザーユニットで管理する**

   ```ini
   # ~/.config/systemd/user/kabekami.service
   [Unit]
   Description=kabekami wallpaper rotator
   After=graphical-session.target plasma-plasmashell.service

   [Service]
   ExecStart=%h/.local/bin/kabekami
   Restart=on-failure
   RestartSec=5

   [Install]
   WantedBy=graphical-session.target
   ```

   ```bash
   systemctl --user enable --now kabekami.service
   # ログを確認する場合
   journalctl --user -u kabekami.service -f
   ```

   > systemd 方式のほうがクラッシュ時の自動再起動（`Restart=on-failure`）や
   > ログ管理が容易です。`plasma-plasmashell.service` を `After` に指定することで
   > トレイの準備ができてから起動します。

## GUI 設定ツール（`kabekami-config`）

`kabekami-config` は kabekami に同梱されたグラフィカルな設定エディターです。

**システムトレイから起動する:**

トレイアイコンを右クリック → **設定を開く**

**コマンドラインから直接起動する:**

```bash
kabekami-config
```

### タブ一覧

| タブ | 内容 |
|------|------|
| **Sources** | 壁紙ディレクトリの追加 / 削除、再帰スキャンの切り替え、お気に入りフォルダの設定 |
| **Rotation** | 切り替え間隔、順次 / ランダム順、起動時即時切り替え、先読み |
| **Display** | 表示モード選択（BlurPad / Fill / Fit / Stretch / Smart）、ぼかし強度・背景暗さのスライダー（**リアルタイムプレビュー付き**） |
| **Cache** | キャッシュディレクトリのパス、最大サイズ（MB）、キャッシュクリア |
| **UI** | 表示言語（`en` / `ja` / `kansai`）、警告のデスクトップ通知 |
| **Online** | オンラインプロバイダーの追加 / 削除（Bing / Unsplash / Wallhaven / Reddit）、API キー、取得間隔、ダウンロード先ディレクトリ |

「**保存 / Save**」ボタンをクリックすると `~/.config/kabekami/config.toml` に書き出されます。
起動中のデーモンは inotify でファイル変更を検知し、再起動なしで自動的に再読み込みします。

> **Note** Display タブのリアルタイムプレビューは 480×270（16:9）で描画します。
> 加工処理はバックグラウンドスレッドで実行されるため UI はブロックされません。

## 設定ファイルリファレンス

設定ファイルのパス: `~/.config/kabekami/config.toml`

設定ファイルが存在しない場合はすべてデフォルト値で起動します。

### `[sources]` — 画像ソース

```toml
[sources]
# 壁紙画像を格納したディレクトリ（複数指定可）
directories = [
    "~/Pictures/Wallpapers",
    "~/Pictures/Photos",
]
# サブディレクトリを再帰的に走査するか（デフォルト: true）
recursive = true

# お気に入りフォルダ — トレイメニューまたは --copy-to-favorites で現在の壁紙をここにコピー
# 未設定の場合は「お気に入りに追加」メニュー項目が無効になります
# favorites_dir = "~/Pictures/Favorites"
```

対応拡張子: `jpg` / `jpeg` / `png` / `webp` / `bmp` / `tiff` / `gif`

### `[rotation]` — 切り替え設定

```toml
[rotation]
# 切り替え間隔（秒）。最小値は 5 秒（下限未満は自動補正）
interval_secs = 1800

# 切り替え順序
#   "random"     — Fisher-Yates シャッフル（全画像を一巡してから再シャッフル）
#   "sequential" — ディレクトリ走査順に順次切り替え
order = "random"

# 起動直後に壁紙を即時切り替えるか（デフォルト: true）
change_on_start = true

# 次の壁紙を事前加工しておくか（短い間隔のときに有効）（デフォルト: true）
prefetch = true
```

### `[display]` — 表示モード

```toml
[display]
# 表示モード（詳細は下記参照）
mode = "blur_pad"

# BlurPad 用パラメータ
blur_sigma = 25.0        # ぼかし強度（大きいほどぼける、推奨: 15〜30）
bg_darken  = 0.1         # 背景を暗くする割合（0.0〜1.0、0.1 = 10%暗く）
```

#### 表示モード一覧

| モード | 動作 |
|---|---|
| `blur_pad` | 元画像をぼかした背景＋前景オーバーレイ（**推奨**） |
| `fill` | 画面を埋める（はみ出し部分を切り取り） |
| `fit` | 画面に収める（余白は黒） |
| `stretch` | アスペクト比無視で引き伸ばし |
| `smart` | アスペクト比の差が小さければ `fill`、大きければ `blur_pad` を自動選択 |

### `[cache]` — キャッシュ設定

```toml
[cache]
# 加工済み画像の保存先（デフォルト: ~/.cache/kabekami）
directory = "~/.cache/kabekami"
# キャッシュの最大サイズ（MB）。超えたら古いファイルから削除（デフォルト: 500）
max_size_mb = 500
```

### `[ui]` — 表示言語

```toml
[ui]
# 表示言語: "en"（英語、デフォルト）/ "ja"（日本語）/ "kansai"（関西弁）
# 環境変数 KABEKAMI_LANG で実行時に上書き可能
language = "ja"
# WARN レベルのログをデスクトップ通知として表示する（デフォルト: false）
warn_notify = false
```

### `[[online_sources]]` — オンライン壁紙ソース

`[[online_sources]]` 配列の各エントリがオンラインプロバイダー 1 件の設定です。
ダウンロード先はデフォルトで `~/.local/share/kabekami/<provider>/` に保存されます。

```toml
# Bing Daily — API キー不要、1 日最大 8 枚
[[online_sources]]
provider = "bing"
enabled  = true
count    = 8            # 1〜8（Bing API 上限）
locale   = "ja-JP"      # 省略時は "en-US"（例: "ja-JP", "de-DE"）
# download_dir = "~/.local/share/kabekami/bing"   # ダウンロード先を変更する場合

# Unsplash — API キー必須（無料プランは 50 リクエスト/時間）
[[online_sources]]
provider = "unsplash"
enabled  = true
api_key  = "YOUR_ACCESS_KEY"
query    = "nature landscape"   # 検索キーワード（省略時は "wallpaper"）
count    = 10                   # 1〜30
# quality = "regular"           # "regular"（デフォルト、約 1080p）または "full"（高解像度・大容量）

# Wallhaven — API キーは任意（NSFW コンテンツ閲覧時のみ必要）
[[online_sources]]
provider = "wallhaven"
enabled  = true
# api_key = "YOUR_API_KEY"
query    = "anime landscape"
count    = 10                   # 1〜24

# Reddit — API キー不要
[[online_sources]]
provider       = "reddit"
enabled        = true
subreddit      = "wallpapers"   # サブレディット名（英数字とアンダースコアのみ）
count          = 10
interval_hours = 1              # 取得間隔の上書き（省略時は Reddit のデフォルト: 1 時間）
```

#### プロバイダーのデフォルト値

| プロバイダー | デフォルト間隔 | 最大件数 | 備考 |
|------------|------------|---------|------|
| `bing` | 24 時間 | 8 | 画面サイズに応じて FHD または UHD を自動選択 |
| `unsplash` | 24 時間 | 30 | `quality = "regular"` 推奨（`"full"` は数十 MB になる場合あり） |
| `wallhaven` | 24 時間 | 24 | デフォルトは SFW のみ（`purity = 100`） |
| `reddit` | 1 時間 | 100 | 直接リンクの画像のみ対応（ギャラリー・アルバムリンクは除外） |

取得インターバルのタイムスタンプは画像が 1 枚以上ダウンロードされたときのみ更新されます。
インターバルを無視してすぐに取得したい場合はトレイメニューの **今すぐ取得** を使ってください。

## 使い方

### 環境変数

| 環境変数 | 説明 |
|---|---|
| `KABEKAMI_SCREEN=2560x1440` | 画面解像度を手動指定（`kscreen-doctor` で自動取得できない場合） |
| `KABEKAMI_LANG=ja` | 表示言語を実行時に上書き（`en` / `ja` / `kansai`） |
| `RUST_LOG=kabekami=debug` | デバッグログを有効化 |

**例:**

```bash
# 4K モニター向けに解像度を指定して起動
KABEKAMI_SCREEN=3840x2160 kabekami

# 英語メニューで起動
KABEKAMI_LANG=en kabekami

# デバッグログを有効にして起動
RUST_LOG=kabekami=debug kabekami
```

### システムトレイメニュー

起動後、システムトレイのアイコンを右クリックするとコンテキストメニューが表示されます。

```
kabekami
├── 次の壁紙              — 即座に次の壁紙へ切り替え（タイマーもリセット）
├── 前の壁紙              — 直前の壁紙に戻る（最大 50 枚分の履歴）
├── ─────────────────────
├── 一時停止 / 再開        — タイマー自動切り替えを止める / 再開する
├── ─────────────────────
├── 表示モード ▶          — Fill / Fit / Stretch / BlurPad / Smart から選択
├── 切り替え間隔 ▶        — 10秒 / 30秒 / 5分 / 30分 / 1時間 / 3時間
├── ─────────────────────
├── 現在の壁紙を開く       — xdg-open で現在の壁紙ファイルを開く
├── お気に入りに追加       — 現在の壁紙を favorites_dir にコピー（未設定時は無効）
├── ゴミ箱に移動          — 現在の壁紙をゴミ箱へ移動し次の壁紙へ
├── 設定を再読み込み       — 再起動なしで config.toml を再読み込み
├── 設定を開く            — kabekami-config GUI を起動
├── 今すぐ取得            — オンラインソースを即時取得（インターバル無視）
├── ─────────────────────
└── 終了
```

> `KABEKAMI_LANG=ja`（または `language = "ja"`）を設定すると日本語メニューで表示されます。

### CLI コマンド

デーモン起動中はコマンドラインから操作できます:

```bash
kabekami --next               # 次の壁紙へ切り替え
kabekami --prev               # 前の壁紙に戻る
kabekami --toggle-pause       # 自動切り替えの一時停止 / 再開
kabekami --reload-config      # config.toml を再読み込み（再起動不要）
kabekami --fetch-now          # オンラインソースを即時取得
kabekami --trash-current      # 現在の壁紙をゴミ箱に移動して次へ
kabekami --copy-to-favorites  # 現在の壁紙をお気に入りフォルダにコピー
kabekami --quit               # デーモンを終了
```

コマンドは D-Bus（`org.kabekami.Daemon`）経由でデーモンに転送されます。
デーモンが起動していない場合はエラーになります。

### 終了する

```bash
# コマンドラインから終了
kabekami --quit

# フォアグラウンドで起動中の場合
Ctrl-C
```

## マルチモニター対応

kabekami は `kscreen-doctor --outputs` で接続・有効なモニターを自動検出し、各画面の解像度に最適化した加工済み画像を個別に適用します。キャッシュキーには画面解像度が含まれるため、モニターごとに個別にキャッシュされます。

`kscreen-doctor` が利用できない場合はプライマリ解像度（または `KABEKAMI_SCREEN` の値）にフォールバックします。

起動時に検出されたモニターを確認するには:

```bash
RUST_LOG=kabekami=info kabekami 2>&1 | grep "monitor detected"
# monitor detected: DP-1 2560x1440
# monitor detected: HDMI-1 1920x1080
```

## セッション管理

kabekami は以下の D-Bus シグナルでシステムセッションと連携します:

| シグナル | 動作 |
|--------|------|
| `org.freedesktop.login1.Manager::PrepareForShutdown(true)` | グレースフルシャットダウン — セッション終了前に状態を保存して終了 |
| `org.freedesktop.DBus::NameOwnerChanged`（`org.kde.plasmashell`） | Plasma 再起動検知 — Plasma の再起動後に現在の壁紙を自動再適用 |

これにより、Plasma がクラッシュした場合や `plasmashell --replace` を実行した場合も壁紙が正しく復元されます。

## ログ

kabekami は `tracing` クレートを使ってログを出力します。
デフォルトでは `INFO` レベル以上が `stderr` に出力されます。

```bash
# ログレベルを変更する（trace / debug / info / warn / error）
RUST_LOG=kabekami=debug kabekami

# 全クレートのログを出力する
RUST_LOG=debug kabekami
```

## キャッシュについて

加工済み画像は `~/.cache/kabekami/` に WebP（ロスレス）で保存されます。
キャッシュキーは以下の組み合わせの SHA256 ハッシュです:

- 元画像の絶対パス
- 画面解像度（マルチモニター時はモニターごとに個別）
- 表示モード
- `blur_sigma` / `bg_darken` の値

**同じ画像・同じ設定であれば再起動後もキャッシュがヒット**するため、
2 回目以降の切り替えは非常に高速です。

キャッシュのクリアは `kabekami-config` の Cache タブの **クリア / Clear Cache** ボタン、または手動で行えます:

```bash
rm -rf ~/.cache/kabekami/
```

## リポジトリ構成

```
kabekami/
├── src/                     # kabekami デーモン
│   ├── main.rs
│   ├── config.rs            # kabekami-common::config の再エクスポート
│   ├── display_mode.rs      # kabekami-common::display_mode の再エクスポート
│   ├── plasma.rs            # KDE Plasma D-Bus / CLI 連携
│   ├── screen.rs            # モニター検出（kscreen-doctor）
│   ├── session.rs           # logind + NameOwnerChanged ウォッチャー
│   └── ...
├── crates/
│   ├── kabekami-common/     # 共有ライブラリ（設定型・画像処理）
│   └── kabekami-config/     # GUI 設定ツール（egui / eframe）
└── Cargo.toml               # Cargo ワークスペースルート
```

## トラブルシューティング

### トレイアイコンが表示されない

- `org.kde.StatusNotifierWatcher` が動作しているか確認してください。
  KDE Plasma が起動している環境では通常自動で起動します。
- Plasma が完全に起動する前に kabekami を起動した場合、
  `kabekami` を再起動してください。
- GNOME など SNI 非対応の DE では KStatusNotifierItem 対応プラグインが必要です。

### `plasma-apply-wallpaperimage` が見つからない

```bash
which plasma-apply-wallpaperimage
```

見つからない場合はパッケージをインストールしてください:

```bash
# Arch Linux
sudo pacman -S plasma-workspace

# Fedora
sudo dnf install plasma-workspace

# Debian / Ubuntu
sudo apt install plasma-workspace
```

### 壁紙が切り替わらない（evaluateScript エラー）

Plasma のウィジェットがロックされているときは `evaluateScript` が失敗することがあります。
デスクトップのロックを解除してから再試行してください。

### マルチモニター: 全画面に同じ画像が表示される

`kscreen-doctor` が `PATH` にない場合、kabekami は個別のモニターを検出できず全画面に同一画像を適用します。`kscreen` をインストールしてください:

```bash
# Arch Linux
sudo pacman -S kscreen

# Fedora
sudo dnf install kscreen

# Debian / Ubuntu
sudo apt install kscreen
```

### 画像の加工が遅い（4K 環境）

BlurPad 加工は内部で 1/4 サイズでぼかし処理を行うため通常 1〜2 秒で完了しますが、
`prefetch = true` にしておくと**次の画像を事前加工**するため切り替え時は即座に反映されます。

### kabekami-config で保存しても反映されない

デーモンは inotify で `config.toml` の変更を検知し自動で再読み込みします。
デーモンが起動していない場合は次回起動時に反映されます。

### オンラインソースの画像が 0 枚しかダウンロードされない

- **Unsplash**: `api_key` が設定されているか、無料プランの 50 リクエスト/時間の上限に達していないか確認してください。
- **Reddit**: サブレディットが存在し、直接リンクの画像投稿（`.jpg` / `.png` / `.webp` で終わる URL、または `post_hint = "image"` の投稿）があることを確認してください。imgur ギャラリーや動画投稿は対象外です。
- **Wallhaven / Bing**: ネットワーク接続を確認してください。`RUST_LOG=kabekami=debug kabekami` で詳細なエラーを確認できます。
- `.last_fetch` タイムスタンプは画像が 1 枚以上ダウンロードされたときのみ更新されます。0 枚の場合は次のインターバルで再試行されます。

### ダウンロード済みの画像がローテーションに表示されない

トレイメニューの **今すぐ取得** で即時取得を試み、その後 **設定を再読み込み** でダウンロードディレクトリを再スキャンしてください。

## ライセンス

[MIT License](LICENSE)

---

## Acknowledgments

kabekami は [Variety](https://github.com/varietywalls/variety) に強くインスパイアされたプロジェクトです。Variety の作者である **Peter Levi** 氏、および長年にわたりメンテナンスを続けてきたコントリビューターの皆さんに深く感謝します。
