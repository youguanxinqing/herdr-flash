# Herdr Flash

[English](README.md) · [简体中文](README.zh-CN.md) · **日本語**

Herdr Flash は、[flash.nvim](https://github.com/folke/flash.nvim) のコピー操作をターミナルのペインに持ち込む [Herdr](https://herdr.dev) プラグインです。表示中のテキストを検索し、ラベルでマッチへジャンプし、vim のモーションで選択して、システムのクリップボードへ yank します。

ヒント型のピッカーは「何をトークンと見なすか」を**あなたの代わりに**決めてしまいます。Flash はそれを二つの問いに分けます。検索が答えるのは**どこを見るか**、モーションが答えるのは**何を取るか**。数文字を打ち、ヒットにカーソルを落とし、欲しいテキストだけを正確に引き抜く — 単語ひとつ、URL の半分、3 行分。

<table>
<tr>
<td width="33%" align="center" valign="top">
<a href="docs/images/01-search.png"><img src="docs/images/01-search.png" width="280" alt="検索フェーズ：クエリのマッチがすべて強調表示され、各ヒットの直後に 1 キーのラベルが付く"></a>
<br><sub><b>1 · 検索</b><br><code>mi</code> と打つとペイン内のヒットがすべて点灯し、その直後に 1 キーのラベルが並びます。件数はステータス行に出ます。</sub>
</td>
<td width="33%" align="center" valign="top">
<a href="docs/images/02-char-jump.png"><img src="docs/images/02-char-jump.png" width="280" alt="文字ジャンプ：f v でカーソルより先のすべての v がラベル付きのターゲットになる"></a>
<br><sub><b>2 · 文字でジャンプ</b><br>カーソルを置いたあと <code>f v</code> で、その先のすべての <code>v</code> がラベル付きターゲットに。1 キーで着地、他のキーでキャンセル。</sub>
</td>
<td width="33%" align="center" valign="top">
<a href="docs/images/03-select-yank.png"><img src="docs/images/03-select-yank.png" width="280" alt="選択フェーズ：複数行にまたがる文字単位の選択範囲、yank 直前の状態"></a>
<br><sub><b>3 · 選択して yank</b><br><code>v</code> で文字単位の選択を開始し、vim モーションで行をまたいで広げ、<code>y</code> でクリップボードへコピー。</sub>
</td>
</tr>
</table>

## 操作の流れ

1. **検索** — フォーカス中のペインでアクションを起動し、そのまま入力します。マッチはすべて強調表示され、各ヒットの直後に 1 文字のラベルが現れます。Backspace でクエリを緩め、Enter で最初のマッチへジャンプします。
2. **カーソル** — マッチのラベル（または Enter）を押すと、そのヒットの先頭文字にカーソルが着地します。この時点ではまだ何も選択されていません。
3. **選択して yank** — `v` で文字単位、`V` で行単位の選択を開始します。vim のモーションで広げてから、`y`（または Enter）で選択範囲をシステムのクリップボードにコピーします。

どのフェーズでも Escape か Ctrl-C で抜けられます。

### キー

| フェーズ | キー | 動作 |
|----------|------|------|
| 検索 | 任意の文字 | クエリを伸ばす（ラベルはジャンプとして消費され、クエリ文字にはならない） |
| 検索 | Backspace | クエリを縮める |
| 検索 | Enter | 最初のマッチへジャンプ |
| カーソル / 選択 | `h j k l` | セル単位の移動 |
| カーソル / 選択 | `w b e` | 単語の前 / 後 / 末尾 |
| カーソル / 選択 | `0 $` | 行頭 / 行末 |
| カーソル / 選択 | `t{char}` `f{char}` | 文字の till / find。前方へ、行をまたいで探す |
| カーソル / 選択 | `T{char}` `F{char}` | 同じものをカーソルから後方へ探す |
| カーソル / 選択 | `v` / `V` | 文字単位 / 行単位の選択を開始（もう一度で解除） |
| 選択 | `o` | カーソル側とアンカー側を入れ替える |
| 選択 | `y` または Enter | 選択範囲を yank（選択がなければ何も起きない） |
| カーソル / 選択 | Backspace | クエリを保ったまま検索へ戻る |
| どこでも | Esc / Ctrl-C | コピーせずに終了 |

`t`/`f`/`T`/`F` のヒットが 1 つなら、そのまま飛びます。複数あれば各ヒットに 1 キーのラベルが付き、ラベルで選択、他のキーでキャンセル。ヒット数がラベルのアルファベットを超えた場合は、`Space` でまだ覆われていないヒットへラベルを送り、巡回します。

押したキーがたまたま表示中のラベルだった場合は、クエリを伸ばさずジャンプします。Backspace を一度押せば、クエリを保ったまま検索に戻ります。

## 設定

既定では yank するとピッカーが閉じます。続けて何度も取りたい場合は（Escape で終了）：

```bash
CONFIG_DIR="$(herdr plugin config-dir youguanxinqing.herdr-flash)"
$EDITOR "$CONFIG_DIR/config.toml"
```

```toml
[flash]
exit_on_yank = false
```

コピーのたびにステータス行で確認が出て、検索は次の取得に向けてリセットされます。

### 配色

既定のパレットは flash.nvim の見た目です。ピッカーが描くものは 5 つのスタイルで足ります。

| スタイル | 何を塗るか | 既定値 |
|----------|-----------|--------|
| `unmatched` | マッチ周辺のペインのテキスト。ヒットが際立つよう暗くする | グレー `#7a8294` |
| `match` | 現在のクエリのすべてのヒットと、ステータス行のクエリ | 青地に白 `#3e68d7` |
| `label` | 各ヒットの直後に描くジャンプキーと、`flash` チップ | マゼンタ地に白 `#ff007c`、太字 |
| `selection` | 有効な `v`/`V` 選択範囲の本体 | 暮色の背景 `#4d3a4a` |
| `cursor` | カーソル / 選択範囲の可動端 | 白地に黒 |

同じ設定ファイルの `[colors]` でどれでも上書きできます。各スタイルは `fg` と `bg` を取り、値は `"#rrggbb"` の 16 進数か、そのチャンネルを端末の既定に戻す `"none"`。加えて `bold` の真偽値があります。

```toml
[colors]
unmatched = { fg = "#6f7788" }               # 背景のテキストをさらに暗く
label = { bg = "#e91e63" }                   # やわらかいピンク。fg と bold は既定のまま
match = { bg = "none", fg = "#e5c07b" }      # 塗りつぶしなし — ただの黄色い文字に
```

書かなかったスタイルとキーは既定のままです。不正な値はピッカーを壊さず、stderr に警告を出して無視されます。色は 24 ビットで、Herdr と現代のあらゆる端末が対応しています。

## 必要なもの

- Herdr 0.7.4 以降
- Rust/Cargo（現状インストールはソースからのビルド。ビルド済みのリリース成果物はまだありません）
- システムのクリップボードコマンド：
    - macOS: `pbcopy`
    - Linux Wayland: `wl-copy`
    - Linux X11: `xclip` または `xsel`

## インストール

```bash
herdr plugin install youguanxinqing/herdr-flash
```

特定のブランチ・タグ・コミットを入れるには `--ref` を渡します。ローカルのチェックアウトからは：

```bash
herdr plugin link .
```

Herdr がアクションを認識しているか確認：

```bash
herdr plugin action list --plugin youguanxinqing.herdr-flash
```

削除するには `herdr plugin uninstall youguanxinqing.herdr-flash`（link したチェックアウトなら `herdr plugin unlink youguanxinqing.herdr-flash`）。

## キーバインド

Herdr の設定に `plugin_action` のバインドを追加して、`herdr server reload-config`：

```toml
[[keys.command]]
key = "prefix+s"
type = "plugin_action"
command = "youguanxinqing.herdr-flash.flash"
description = "flash: search visible text, then select and yank"
```

## 開発メモ

込み入った回帰と、それを防ぐための不変条件は
[`docs/bugs/`](docs/bugs/README.md) に記録しています。とくに
[Flash picker entry blink](docs/bugs/flash-picker-entry-blink.md) は、隠しタブの描画ハンドシェイクと、
ソース側とピッカー側でジオメトリを分けている理由を扱っています。

## クレジット

- folke による [flash.nvim](https://github.com/folke/flash.nvim) — このプラグインが敬意を表している操作モデル。検索で動き、ラベルで飛ぶ。カーソルを落とすことと、取るテキストを決めることを別の行為として扱う考え方です。
- rmarganti による [herdr-pluck](https://github.com/rmarganti/herdr-pluck) — Herdr Flash はこれの fork として始まりました。ペインのスナップショット、ピッカーの土台、レンダラーは今もその仕事の上に立っています。URL・SHA・パスのような形の決まったトークンを 1 打で取りたいなら pluck が適任です。Flash が向くのは、中身は分かっているが形が分からないときです。

## ライセンス

MIT — [LICENSE](LICENSE) を参照。
