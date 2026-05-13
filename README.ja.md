> [English README is here / 英語版はこちら](README.md)

# kabekami（壁紙）

KDE Plasma 向け壁紙ローテーションデーモン。Rust 製。

- ローカル画像をタイマーで順次／ランダム切り替え（システムトレイから操作）
- **BlurPad** モード: ぼかした背景の中央に元画像をオーバーレイ（[Variety](https://github.com/varietywalls/variety) の blur-pad 相当）
- **マルチモニター**: `kscreen-doctor` で各画面の解像度に最適化した画像を個別適用
- **オンラインソース**: Bing Daily・Unsplash・Wallhaven・Reddit をスケジュール自動取得
- **GUI 設定ツール**（`kabekami-config`）: BlurPad リアルタイムプレビュー付き

## 動作要件

| 項目 | 要件 |
|---|---|
| OS | Linux |
| DE | KDE Plasma 5.7 以降 または Plasma 6 |
| Rust | 1.75 以降（edition 2021） |
| 外部コマンド | `plasma-apply-wallpaperimage`（`plasma-workspace` 同梱） |
| D-Bus | セッションバスへのアクセス（トレイ表示に必要） |
| `kscreen-doctor` | 任意 — マルチモニター自動検出に必要（`kscreen` パッケージ） |
| `kdialog` | 任意 — `kabekami-config` でネイティブな KDE ファイル／フォルダ選択ダイアログを使用 |

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

   > `X-KDE-autostart-phase=2` により Plasma の初期化完了後に起動するためトレイアイコンが確実に表示されます。

   クラッシュ時の自動再起動が必要な場合は **systemd ユーザーユニット** を使用できます:

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
   journalctl --user -u kabekami.service -f   # ログを確認
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
├── 設定を開く            — kabekami-config を起動
└── 終了
```

### CLI コマンド

```bash
kabekami --next
kabekami --prev
kabekami --toggle-pause
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

## 設定ファイル

設定ファイルのパス: `~/.config/kabekami/config.toml`

全設定項目とデフォルト値の詳細は、リポジトリの [`config.ja.toml`](config.ja.toml) を参照してください。

### 対応画像形式

kabekami は以下の画像形式に対応しています: **jpg, jpeg, png, webp, avif**

EXIF Orientation タグは自動的に読み取り・適用されるため、縦撮り写真や回転情報を持つ画像も正しい向きで表示されます。

注意: bmp, tiff, gif には対応していません（バイナリサイズ削減のため `image` クレートの feature を jpeg/png/webp/avif に限定しています）。

## トラブルシューティング

**トレイアイコンが表示されない** — Plasma が完全に起動してから kabekami を再起動してください。

**`plasma-apply-wallpaperimage` が見つからない** — `plasma-workspace` をインストールしてください。

**壁紙が切り替わらない（evaluateScript エラー）** — デスクトップのロックを解除してから再試行してください。

**マルチモニターで全画面に同じ画像が表示される** — `kscreen` をインストールしてください。

**壁紙がぼやける／ネイティブ解像度で表示されない** — `kscreen-doctor` が利用できないか、出力の解析に失敗した場合、kabekami は 1920×1080 にフォールバックします。`kscreen` パッケージをインストールするか、`KABEKAMI_SCREEN=2560x1440`（実際の解像度に置き換えてください）を環境変数で指定してから再起動してください。

**オンラインソースが 0 枚しかダウンロードされない** — API キー・ネットワーク・`RUST_LOG=kabekami=debug` の出力を確認してください。

**kabekami-config で保存しても反映されない** — デーモンは inotify で `config.toml` の変更を自動検知します。反映されない場合は再起動してください。

## ライセンス

[MIT License](LICENSE)

---

[Variety](https://github.com/varietywalls/variety) に強くインスパイアされています。作者の Peter Levi 氏とコントリビューターの皆さんに感謝します。
