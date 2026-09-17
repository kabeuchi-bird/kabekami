# AUR への公開手順

AUR は `aur.archlinux.org` の git リポジトリだけを見ている。GitHub 側を更新しても
`paru -S kabekami-git` の内容は変わらない。

ソースコードの変更は `-git` パッケージなので自動で反映される（`pkgver()` が動的）。
**この手順が必要なのは `PKGBUILD` / `kabekami.install` / `kabekami-config.desktop`
を変更したときだけ。**

## 初回

1. https://aur.archlinux.org のアカウント設定（My Account → SSH Public Key）に
   SSH 公開鍵を登録する。

2. クローンする。AUR のリポジトリは**平置き**（ファイルが直下に来る）。

   ```bash
   git clone ssh://aur@aur.archlinux.org/kabekami-git.git ~/aur/kabekami-git
   ```

3. ファイルをコピーして `.SRCINFO` を生成し、ビルドを通してから push する。

   ```bash
   cd ~/aur/kabekami-git
   cp ~/kabekami/packaging/aur/kabekami-git/{PKGBUILD,kabekami.install,kabekami-config.desktop} .
   makepkg --printsrcinfo > .SRCINFO
   makepkg -si                      # 手元でビルドが通ることを確認
   git add -A
   git commit -m "initial import"
   git push
   ```

`.SRCINFO` は AUR 側のフックが PKGBUILD との一致を検証する。**手書きすると push が
弾かれる**ので必ず `makepkg --printsrcinfo` で生成する。

## 二回目以降

```bash
cd ~/aur/kabekami-git
git pull
cp ~/kabekami/packaging/aur/kabekami-git/{PKGBUILD,kabekami.install,kabekami-config.desktop} .
makepkg --printsrcinfo > .SRCINFO
makepkg -si
git add -A && git commit -m "変更内容" && git push
```

push 前に確認すること:

- **`kabekami.install` か `.desktop` を編集したら `PKGBUILD` の `sha256sums` を更新する。**
  古いままだと `makepkg` の検証で落ちて誰もインストールできなくなる。

  ```bash
  sha256sum kabekami.install kabekami-config.desktop
  ```

- **パッケージング側だけを変えたときは `pkgrel` を +1 する。**
  ソースコードだけの変更なら `pkgver()` が動くので不要。
