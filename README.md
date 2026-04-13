# kabekami

KDE Plasma 向け壁紙ローテーションツール。Rust 製。

- ローカル画像をタイマーで順次 / ランダム切り替え
- **BlurPad** モード: 元画像をぼかした背景の中央に原画をオーバーレイ（[Variety](https://github.com/varietywalls/variety) の blur-pad に相当）
- システムトレイ常駐（SNI プロトコル）＋コンテキストメニュー操作
- 加工済み画像のキャッシュ（SHA256 キー、LRU 退避）で短い間隔でも高速
- 次の画像を事前にバックグラウンド加工しておく先読み機構

## 動作要件

| 項目 | 要件 |
|---|---|
| OS | Linux |
| DE | KDE Plasma 5.7 以降 または Plasma 6 |
| Rust | 1.75 以降（edition 2021） |
| 外部コマンド | `plasma-apply-wallpaperimage`（Plasma 付属） |
| D-Bus | セッションバスへのアクセス（トレイ表示に必要） |

> **Note** `plasma-apply-wallpaperimage` は KDE パッケージに同梱されています。
> Arch Linux では `plasma-workspace`、Fedora/Debian では `plasma-workspace` または
> `kde-plasma-desktop` に含まれています。

## インストール

### cargo build（推奨）

```bash
git clone https://github.com/kabeuchi-bird/kabekami.git
cd kabekami
cargo build --release
# バイナリは target/release/kabekami に生成される
sudo install -m755 target/release/kabekami /usr/local/bin/
```

### cargo install（crates.io 公開後）

```bash
cargo install kabekami
```

### AUR（Arch Linux）

```bash
# 公開後
paru -S kabekami
# または
yay -S kabekami
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
   # ~/.config/autostart/ に .desktop ファイルを作成する
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

| モード | 動作 | 実装状況 |
|---|---|---|
| `blur_pad` | 元画像をぼかした背景＋前景オーバーレイ（**推奨**） | Phase 2 ✓ |
| `fill` | 画面を埋める（はみ出し部分を切り取り） | Phase 3 予定 ※ |
| `fit` | 画面に収める（余白は黒） | Phase 3 予定 ※ |
| `stretch` | アスペクト比無視で引き伸ばし | Phase 3 予定 ※ |
| `smart` | アスペクト比の差が小さければ `fill`、大きければ `blur_pad` を自動選択 | Phase 3 予定 ※ |

> ※ Phase 3 実装前は `blur_pad` にフォールバックします。

### `[cache]` — キャッシュ設定

```toml
[cache]
# 加工済み画像の保存先（デフォルト: ~/.cache/kabekami）
directory = "~/.cache/kabekami"
# キャッシュの最大サイズ（MB）。超えたら古いファイルから削除（デフォルト: 500）
max_size_mb = 500
```

## 使い方

### 起動オプション

```
kabekami [OPTIONS]
```

| 環境変数 | 説明 |
|---|---|
| `KABEKAMI_SCREEN=2560x1440` | 画面解像度を手動指定（Phase 3 で自動取得予定） |
| `RUST_LOG=kabekami=debug` | デバッグログを有効化 |

**例:**

```bash
# 4K モニター向けに解像度を指定して起動
KABEKAMI_SCREEN=3840x2160 kabekami

# デバッグログを有効にして起動
RUST_LOG=kabekami=debug kabekami
```

> **Note** Phase 3 では `kscreen-doctor` を使って画面解像度を自動取得する予定です。
> それまでは `KABEKAMI_SCREEN` 環境変数で設定してください。デフォルト: `1920x1080`

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
├── ─────────────────────
└── 終了
```

### Ctrl-C で終了

```bash
# フォアグラウンドで起動中の場合
Ctrl-C
```

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

加工済み画像は `~/.cache/kabekami/` に JPEG（品質 92）で保存されます。
キャッシュキーは以下の組み合わせの SHA256 ハッシュです:

- 元画像の絶対パス
- 画面解像度
- 表示モード
- `blur_sigma` / `bg_darken` の値

**同じ画像・同じ設定であれば再起動後もキャッシュがヒット**するため、
2 回目以降の切り替えは非常に高速です。

キャッシュを手動でクリアする場合:

```bash
rm -rf ~/.cache/kabekami/
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

### 画像の加工が遅い（4K 環境）

BlurPad 加工は内部で 1/4 サイズでぼかし処理を行うため通常 1〜2 秒で完了しますが、
`prefetch = true` にしておくと**次の画像を事前加工**するため切り替え時は即座に反映されます。

## ライセンス

[MIT License](LICENSE)

---

## Acknowledgments

kabekami は [Variety](https://github.com/varietywalls/variety) に強くインスパイアされたプロジェクトです。Variety の作者である **Peter Levi** 氏、および長年にわたりメンテナンスを続けてきたコントリビューターの皆さんに深く感謝します。
