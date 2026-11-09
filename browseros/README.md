# Lapce on BrowserOS

このブランチは、Lapce を BrowserOS の Rust の target `wasm32-browseros-linux-musl` で組み、
[BrowserOS](https://github.com/Progate/packages/tree/main/packages/browser-os)（ブラウザーの中で動く小さな OS）の
windowserver に窓を開いて動かすための変更である。使う側は `@progate/browser-editor`。

`wasm32-browseros-linux-musl` は、wasm の上で musl と Linux の syscall（riscv32 と同じ番号と並び）を使う target で、
`target_os = "linux"`・`target_env = "musl"`・`target_vendor = "browseros"` を名乗る（→ Progate/rust の `browseros-1.99`）。
だから Lapce は **Linux 版と同じコード**で動く。子プロセス（`std::process::Command`）・スレッド・pty・inotify（notify）・
Unix ドメインソケット（interprocess）・ゴミ箱（trash）・XDG の置き場（directories）・文法の dlopen（libloading）は、
どれも Linux のものをそのまま使う。

BrowserOS に固有の変更は `#[cfg(target_vendor = "browseros")]` の下にある。編集の振る舞いを VS Code に揃えた変更
（→「編集の振る舞い」）はネイティブ版にも効く。

## 組み方

`@progate/browser-editor` の `scripts/build-wasm.mjs` が組む（rustc・世界の sysroot・リンカーの browser-cc を用意してから
`cargo build --release --target wasm32-browseros-linux-musl --bin lapce`）。手で組むときは同じ環境を渡す:

```sh
export RUSTC=<ホストと wasm32-browseros-linux-musl の std を持つ stage1 の rustc>
export CARGO_TARGET_WASM32_BROWSEROS_LINUX_MUSL_LINKER=<browser-cc の包み>
export CC_wasm32_browseros_linux_musl=<browser-cc の包み>  CFLAGS_wasm32_browseros_linux_musl=-pthread
export WASM_MUSL_SYSROOT=<世界の sysroot>
# 設定ディレクトリを ~/.config/lapce-stable にする（無いと Nightly 扱いになる）
export RELEASE_TAG_NAME=v0.4.6
cargo build --release --target wasm32-browseros-linux-musl --bin lapce
```

出来上がるのは `libc.so` を NEEDED に持つ BrowserOS のプログラムで、カーネルが Linux のプログラムと同じく起こす。

## 画面と入力

BrowserOS の画面を持つのは windowserver というプロセスで、アプリは `DISPLAY`（既定
`127.0.0.1:6000`）へ繋いで窓をもらう。取り決めは「ソケットに 1 行ずつ書く」「画素をファイルに書く」
の 2 つだけである。

- **winit** に `platform_impl/wasi` を足した（Redox の orbital と同じ形。窓 1 つにつき接続 1 本）。
  画素は `platform::wasi::WindowExtWasi::present_rgba` で渡し、前のフレームから変わった行だけを送る。
  BrowserOS では X11・Wayland を選ばない（`build.rs` の `free_unix` から外す）
- **floem** は BrowserOS では GPU を使わず tiny-skia で描く（BrowserOS の中のプロセスから WebGPU は呼べない）
- 倍率は `WINIT_SCALE_FACTOR` で受け取る（画面の画素と見た目の 1px の比はホストが決める）

## ネイティブ版と違うところ

| ネイティブ版 | BrowserOS 版 |
| --- | --- |
| 自分を子プロセスで起動し直して端末を空ける | しない（起動し直すと lapce.wasm の読み込みをもう一度払う） |
| libgit2 | BrowserOS の `git` を子プロセスで呼ぶ（`lapce-proxy/src/git_cli.rs`）。libgit2 と OpenSSL を wasm に焼き込まない |
| 文法をプラグインの置き場から取ってくる | OS が配る文法（`/usr/lib/lapce/grammars`）とハイライトの定義（`/usr/share/lapce/queries`）も読む（`lapce-core/src/directory.rs`） |
| 言語のプラグイン（wasmtime）が言語サーバーを起こす | OS が配るプラグイン（`/usr/share/lapce/plugins`）の `language-server.toml` に書いた言語サーバーを、Lapce が同じ LSP のクライアントで起こす（`lapce-proxy/src/plugin/language_server.rs`）。wasm のプラグインは動かない（wasm の中に wasmtime は置けない） |
| HTTP（プラグインの取得・更新の確認） | 無し |
| ほかのアプリで開く（`open`） | 無し（`xdg-open` が居ない） |
| rayon のスレッドプールは CPU の数だけ | 2 本（`lapce-app/src/app.rs`）。BrowserOS ではスレッド 1 本がワーカー 1 つになり数 MB を食う。rayon で走らせるのは構文解析・検索・差分だけ |
| 更新の確認のスレッドを起こす | 起こさない（OS が配る wasm で自分を差し替えられない） |
| 設定の置き場を notify で見張る | 見張らない。Lapce が設定・割り当てのファイルを書いたときに読み直す（`lapce-app/src/config/watcher.rs`）。ホストは起動の前に書き終えている |

演習の画面に埋め込むための引数（`--read-only`・`--no-title-bar`・`--no-open-editors`・`--no-source-control`）と、
外から全部保存を頼む `--save-all` も BrowserOS 版にある。

## Lapce に足した機能

- **画像のタブ**（`lapce-app/src/image_preview.rs`）。PNG・JPEG・GIF・WebP・BMP・ICO は開くと画像のタブになり、
  元の大きさで（入りきらなければ縮めて）描き、形式・寸法・大きさを下に出す。SVG はエディタで開き、
  エクスプローラーの右クリックかコマンドの `open_image_preview`（画像のプレビューを開く）で画像のタブを出す。
  エディタでも開いているあいだは、保存を待たずに編集中の文字を描く

## 編集の振る舞い

学習者が VS Code と同じつもりで打てるよう、次の 2 つを VS Code に揃えた。

- **補完を開く文字は言語サーバーが決める**（`lapce-app/src/editor.rs` の `update_completion`、
  `lapce-proxy/src/plugin/psp.rs` の `request_wanted`）。単語の途中でない文字（`.` など）を打つと、その文字を
  trigger character として言語サーバーへ頼み、`completionProvider.triggerCharacters` に挙げたサーバーだけが答える。
  単語の候補（`editor.word-based-suggestions`）は単語を打ち始めてから出す。だから Python の `def f():` のように行末を
  `:` で終えて Enter を押すと、候補ではなく改行が入る
- **Backspace で対の閉じも消すのは、自動で閉じた直後だけ**（`browseros/crates/floem/editor-core` の
  `Buffer::mark_auto_closed` と `DeleteBackward`。VS Code の `editor.autoClosingDelete: "auto"`）。元から書いてあった
  `""` の片方や、間に空白がある対では、打った側の 1 文字だけが消える

## 手を入れた依存（`browseros/crates/`）

上流の版に BrowserOS のための変更を足したものを置いている。Lapce の `Cargo.toml` はこれを path で指す。
各 crate の変更はそのディレクトリの中で `target_vendor = "browseros"` を探せば分かる。

| crate | 上流 | 変更 |
| --- | --- | --- |
| winit | rust-windowing/winit `ee245c56` | `platform_impl/wasi`（windowserver の窓と、貼り付け板の selection の控え） |
| floem | lapce/floem `31fa8f44` | GPU の代役・tiny-skia へ画素を渡す口・書体の読み込みと代わりの書体の一覧・斜体の無い書体を斜体としても登録・windowserver の貼り付け板を使うクリップボード（copypasta は trait だけ使い、X11・Wayland を引かない）・窓の地の色（`WindowConfig::background`。既定の白が最初のコマに見えないように）・復号済みの画像を描く `img_dynamic` の公開・SVG の `<text>` に文字の描画と同じ書体を渡す・Backspace で対の閉じも消すのは自動で閉じたものだけ（→「編集の振る舞い」） |
| alacritty_terminal | alacritty/alacritty `cacdb5bb` | 端末のシェルを posix_spawn(3) で起こす（`src/tty/unix/spawn_browseros.rs`）。Linux 版の `pre_exec` は fork(2) を要るが、wasm のプロセスは fork で写せない。`POSIX_SPAWN_SETSID` と、pty の従を fd 0 に開く file actions で同じことをする |
| directories | crates.io 4.0.1 | wasm32 を「何も無い」扱いにして置き場を返さないので、BrowserOS では Linux の XDG の置き場を選ぶ |

crates.io の crate のうち、BrowserOS の分岐を足した Progate の fork を引くもの（`Cargo.toml` の `[patch.crates-io]`）:

| crate | fork のコミット | 変更 |
| --- | --- | --- |
| libc | Progate/rust-libc `1d8b649b` | BrowserOS の型と定数（riscv32 の Linux と同じ並び） |
| rustix 0.38 | Progate/rustix `84ee69a2`（`browseros-0.38.32`） | BrowserOS を riscv32 と同じに扱う（open の syscall が無い・汎用の ioctl の番号） |
| linux-raw-sys 0.4 | Progate/linux-raw-sys `aef24ad2`（`browseros-0.4.13`） | riscv32 の定義を使う |
| backtrace | Progate/backtrace-rs `d61bcefe` | wasm は自分のスタックを歩けないので、Emscripten と同じく何もしない |

ライセンスは各ディレクトリの LICENSE のとおり（winit・alacritty_terminal は Apache-2.0、floem は MIT、
directories は MIT OR Apache-2.0）。
