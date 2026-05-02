> [English README is here / 英語版はこちら](README.md)

# kabekami（壁紙）

KDE Plasma 向け壁紙ローテーションデーモン。Rust 製。

- ローカル画像をタイマーで順次 / ランダム切り替え
- **BlurPad** モード: ぼかした背景の中央に元画像をオーバーレイ（[Variety](https://github.com/varietywalls/variety) の blur-pad 相当）
- システムトレイ常駐（SNI）＋コンテキストメニュー、多言語対応
- LRU キャッシュ＋先読みで短い間隔でも即座に切り替え
- **マルチモニター**: `kscreen-doctor` で各画面の解像度に最適化した画像を個別適用
- **オンラインソース**: Bing Daily・Unsplash・Wallhaven・Reddit をスケジュール自動取得
- **二度と表示しない**: 壁紙を永続ブラックリスト登録（`~/.config/kabekami/blacklist.txt`）
- **グローバルショートカット**: システム設定 → ショートカット → kabekami で設定可能
- **セッション管理**: `logind` でシャットダウン検知・Plasma 再起動時に壁紙を自動再適用
- **GUI 設定ツール**（`kabekami-config`）: egui 製の 6 タブ設定画面。BlurPad リアルタイムプレビュー付き

## 動作要件

| 項目 | 要件 |
|---|---|
| OS | Linux |
| DE | KDE Plasma 5.7 以降 または Plasma 6 |
| Rust | 1.75 以降（edition 2021） |
| 外部コマンド | `plasma-apply-wallpaperimage`（`plasma-workspace` 同梱） |
| D-Bus | セッションバスへのアクセス（トレイ表示に必要） |
| `kscreen-doctor` | 任意 — マルチモニター自動検出に必要（`kscreen` パッケージ） |

## インストール

### ソースからビルド

```bash
git clone https://github.com/kabeuchi-bird/kabekami.git
cd kabekami
cargo build --release
sudo install -m755 target/release/kabekami        /usr/local/bin/
sudo install -m755 target/release/kabekami-config /usr/local/bin/
```

### AUR（Arch Linux）

```bash
paru -S kabekami-git
```

## クイックスタート

1. `~/.config/kabekami/config.toml` を作成:

   ```toml
   [sources]
   directories = ["~/Pictures/Wallpapers"]

   [rotation]
   interval_secs = 1800
   order = "random"

   [display]
   mode = "blur_pad"

   [ui]
   language = "ja"   # "en" または "ja"
   ```

   GUI で設定したい場合は `kabekami-config` を起動してください。

2. `kabekami` を実行 — システムトレイにアイコンが表示されます。

3. **自動起動**（任意）— `.desktop` ファイルを配置:

   ```bash
   cat > ~/.config/autostart/kabekami.desktop <<'EOF'
   [Desktop Entry]
   Name=kabekami
   Exec=kabekami
   Type=Application
   X-KDE-autostart-phase=2
   EOF
   ```

## 使い方

### システムトレイメニュー

```
├── 次の壁紙              — 即座に切り替え（タイマーリセット）
├── 前の壁紙              — 直前の壁紙に戻る（最大 50 枚の履歴）
├── 一時停止 / 再開
├── 表示モード ▶          — Fill / Fit / Stretch / BlurPad / Smart
├── 切り替え間隔 ▶        — 10秒 / 30秒 / 5分 / 30分 / 1時間 / 3時間
├── 現在の壁紙を開く
├── お気に入りに追加       — （favorites_dir 未設定時は無効）
├── ゴミ箱に移動          — 削除して次の壁紙へ
├── 二度と表示しない       — 永続ブラックリスト登録
├── 設定を再読み込み
├── 設定を開く            — kabekami-config を起動
├── 今すぐ取得            — オンラインソースを即時取得
└── 終了
```

### CLI コマンド

```bash
kabekami --next
kabekami --prev
kabekami --toggle-pause
kabekami --reload-config
kabekami --fetch-now
kabekami --trash-current
kabekami --blacklist-current
kabekami --copy-to-favorites
kabekami --quit
```

コマンドは D-Bus（`org.kabekami.Daemon`）経由でデーモンに転送されます。

### グローバルショートカット

**システム設定 → ショートカット → kabekami** で任意のキーを割り当てられます（デフォルトなし）:

| アクション | 説明 |
|-----------|------|
| Next Wallpaper | 次の壁紙へ切り替え |
| Previous Wallpaper | 前の壁紙に戻る |
| Pause / Resume | 自動切り替えのオン / オフ |
| Move to Trash | ゴミ箱へ移動して次へ |
| Never Show Again | 永続ブラックリスト登録 |

### 環境変数

| 環境変数 | 説明 |
|---|---|
| `KABEKAMI_SCREEN=2560x1440` | 画面解像度を手動指定 |
| `KABEKAMI_LANG=ja` | 表示言語を上書き（`en` / `ja`） |
| `RUST_LOG=kabekami=debug` | デバッグログを有効化 |

## 設定ファイルリファレンス

設定ファイルのパス: `~/.config/kabekami/config.toml`（すべての値は省略可、省略時はデフォルト値を使用）

### `[sources]`

```toml
[sources]
directories    = ["~/Pictures/Wallpapers"]
recursive      = true
# favorites_dir = "~/Pictures/Favorites"
```

対応拡張子: `jpg` `jpeg` `png` `webp` `bmp` `tiff` `gif`

### `[rotation]`

```toml
[rotation]
interval_secs   = 1800      # 最小 5 秒
order           = "random"  # "random" または "sequential"
change_on_start = true
prefetch        = true
```

### `[display]`

```toml
[display]
mode       = "blur_pad"  # blur_pad / fill / fit / stretch / smart
blur_sigma = 25.0        # BlurPad ぼかし強度（推奨: 15〜30）
bg_darken  = 0.1         # BlurPad 背景の暗さ（0.0〜1.0）
```

| モード | 動作 |
|---|---|
| `blur_pad` | ぼかした背景＋前景オーバーレイ（**推奨**） |
| `fill` | 画面を埋める（はみ出し部分を切り取り） |
| `fit` | 画面に収める（余白は黒） |
| `stretch` | アスペクト比無視で引き伸ばし |
| `smart` | アスペクト比の差に応じて `fill` / `blur_pad` を自動選択 |

### `[cache]`

```toml
[cache]
directory   = "~/.cache/kabekami"
max_size_mb = 500
```

キャッシュのクリアは **kabekami-config → Cache → Clear Cache**、または `rm -rf ~/.cache/kabekami/`。

### `[ui]`

```toml
[ui]
language    = "ja"    # "en" または "ja"
warn_notify = false   # WARN ログをデスクトップ通知として表示
```

### `[[online_sources]]`

```toml
# Bing Daily（API キー不要）
[[online_sources]]
provider = "bing"
enabled  = true
count    = 8
locale   = "ja-JP"

# Unsplash（API キー必須）
[[online_sources]]
provider = "unsplash"
enabled  = true
api_key  = "YOUR_KEY"
query    = "nature landscape"
count    = 10

# Wallhaven（API キーは任意）
[[online_sources]]
provider = "wallhaven"
enabled  = true
query    = "landscape"
count    = 10

# Reddit（API キー不要）
[[online_sources]]
provider       = "reddit"
enabled        = true
subreddit      = "wallpapers"
count          = 10
interval_hours = 1
```

トレイの **今すぐ取得** でインターバルを無視して即時取得できます。

## トラブルシューティング

**トレイアイコンが表示されない** — Plasma が完全に起動してから kabekami を再起動してください。

**`plasma-apply-wallpaperimage` が見つからない** — `plasma-workspace` をインストールしてください。

**壁紙が切り替わらない（evaluateScript エラー）** — デスクトップのロックを解除してから再試行してください。

**マルチモニターで全画面に同じ画像が表示される** — `kscreen` をインストールしてください。

**オンラインソースが 0 枚しかダウンロードされない** — API キー・ネットワーク・`RUST_LOG=kabekami=debug` の出力を確認してください。

**kabekami-config で保存しても反映されない** — デーモンは inotify で `config.toml` の変更を自動検知します。反映されない場合は再起動してください。

## ライセンス

[MIT License](LICENSE)

---

[Variety](https://github.com/varietywalls/variety) に強くインスパイアされています。作者の Peter Levi 氏とコントリビューターの皆さんに感謝します。
