# Lapce on WASI（BrowserOS）

このブランチ（`wasip1`）は、Lapce を `wasm32-wasip1-threads` の command として組み、
[BrowserOS](https://github.com/Progate/packages/tree/main/packages/browser-os)（ブラウザーの中で動く小さな OS）の
windowserver に窓を開いて動かすための変更である。使う側は `@progate/browser-editor`。

WASI のための変更はすべて `#[cfg(target_os = "wasi")]` の下にあり、**ネイティブ版の振る舞いは変えていない**。
例外は画面の言語（→ 下の「画面の言語」）で、こちらはネイティブ版でも効く。

## 組み方

```sh
rustup target add wasm32-wasip1-threads
# C の依存（tree-sitter・zstd）を組む clang。wasi-sdk 34 で確かめている
export CC_wasm32_wasip1_threads="$WASI_SDK/bin/clang --target=wasm32-wasip1-threads --sysroot=$WASI_SDK/share/wasi-sysroot -pthread"
export AR_wasm32_wasip1_threads=$WASI_SDK/bin/llvm-ar
# 設定ディレクトリを ~/.config/lapce-stable にする（無いと Nightly 扱いになる）
export RELEASE_TAG_NAME=v0.4.6
cargo build --release --target wasm32-wasip1-threads --bin lapce
```

## 画面と入力

BrowserOS の画面を持つのは windowserver というプロセスで、アプリは `DISPLAY`（既定
`127.0.0.1:6000`）へ繋いで窓をもらう。取り決めは「ソケットに 1 行ずつ書く」「画素をファイルに書く」
の 2 つだけである。

- **winit** に `platform_impl/wasi` を足した（Redox の orbital と同じ形。窓 1 つにつき接続 1 本）。
  画素は `platform::wasi::WindowExtWasi::present_rgba` で渡し、前のフレームから変わった行だけを送る
- **floem** は WASI では GPU を使わず tiny-skia で描く（BrowserOS の中のプロセスから WebGPU は呼べない）
- 倍率は `WINIT_SCALE_FACTOR` で受け取る（画面の画素と見た目の 1px の比はホストが決める）

## 画面の言語（本物の Lapce には無い）

メニュー・パネル・コマンド パレット・設定などの画面の文字を、日本語でも出せるようにした（`lapce-app/src/i18n.rs`）。
これは WASI に限らない変更で、ネイティブ版でも効く。

- 言語は設定の `core.language`（`system`・`en`・`ja`）で選ぶ。既定の `system` は、ほかの Linux のプログラムと同じく
  ロケールの環境変数（`LC_ALL`・`LC_MESSAGES`・`LANG`・`LANGUAGE`）に従う。起動する側が `LANG=ja_JP.UTF-8` を渡せば日本語で開き、
  利用者は設定で英語に戻せる
- gettext と同じく、ソースに書いた英語の原文がキーで、訳は `defaults/locales/ja/*.toml` に置く。訳の無い文字は英語のまま出る
- 言語は起動したときに決まる。設定で選び直した言語は、VS Code の表示言語と同じく次に起動したときから効く
  （Lapce は窓のビューとデータを一緒に作るので、動いている途中でビューだけを作り直せない）
- コマンド パレットとキーボード ショートカットは、訳したコマンド名でも英語の名前でも探せる

## OS に無いもの・代わりにしたもの

| ネイティブ版 | WASI 版 |
| --- | --- |
| 自分を子プロセスで起動し直して端末を空ける | しない（子プロセスは std から起動できない） |
| libgit2 | BrowserOS の `git` を Linux の syscall（`clone` の vfork の形と `execve`）で起こす（`lapce-proxy/src/git_wasi.rs`・`wasi_process.rs`。syscall は `linux_wasi.rs`） |
| inotify / FSEvents（notify） | Linux の inotify（`inotify_init1(2)` ほか。`lapce-proxy/src/inotify_wasi.rs`） |
| 文法を dlopen で読む | 同じく `libtree-sitter-<name>.so` を dlopen する。文法は wasm のサイドモジュールで、ローダーは BrowserOS の browser-dyld。利用者の置き場に無ければ OS が配る `/usr/lib/lapce/grammars`・`/usr/share/lapce/queries` を見る（`lapce-core/src/language.rs`・`directory.rs`）。組むのは @progate/browser-editor の `scripts/build-wasm.mjs` |
| 窓の枠と帯 | 枠は WM か埋め込む側が持つので、帯は macOS 版と同じ並びにし、ロゴ・メニュー・窓のボタン・SSH・更新・Run and Debug を出さない（`lapce-app/src/title.rs`） |
| 拡張・LSP・デバッガーの板 | 動かないので並びから外す（種類は残す。`lapce-app/src/panel/kind.rs` の `is_available`） |
| （無い） | `--read-only` でワークスペースごと読み取り専用にする。proxy が書き換えを断り、端末と git を起こさない（`lapce-core/src/read_only.rs`） |
| `directories` | Linux と同じ XDG の置き場（`lapce-core/src/xdg.rs`） |
| ゴミ箱（trash） | freedesktop.org のゴミ箱（`~/.local/share/Trash`）へ自分で移す |
| プラグイン（wasmtime） | 未対応（起動しようとすると理由付きで失敗する） |
| 端末（pty と epoll の event_loop） | Linux の openpty(3) と同じく `/dev/ptmx` を開いた pty の上で、`setsid(2)` してからシェルを起動する（`lapce-proxy/src/terminal_wasi.rs`） |
| HTTP（プラグインの取得・更新の確認） | 未対応 |
| 単一起動の IPC | 無し（毎回自分で窓を開く） |

## 手を入れた依存（`wasip1/crates/`）

上流の版に、WASI のための変更を足したものを置いている。Lapce の `Cargo.toml` はこれを path で指す。
各 crate の変更はそのディレクトリの中で `target_os = "wasi"` を探せば分かる。

| crate | 上流 | 変更 |
| --- | --- | --- |
| winit | rust-windowing/winit `ee245c56` | `platform_impl/wasi`（windowserver の窓と、貼り付け板の selection の控え） |
| floem | lapce/floem `31fa8f44` | GPU の代役・tiny-skia へ画素を渡す口・書体の読み込み（mmap を使わない）と代わりの書体の一覧・斜体の無い書体を斜体としても登録・windowserver の貼り付け板を使うクリップボード |
| alacritty_terminal | alacritty/alacritty `cacdb5bb` | WASI では pty と event_loop を外す（格子と vte はそのまま使う） |
| parking_lot_core | crates.io 0.9.11 | wasm の待ちは nightly の feature でしか本物にならず、ロックが競合すると panic していたので、std の park / unpark（futex）で待つ実装を足した |
| filetime | crates.io 0.2.26 | WASI を「何も無い wasm」と同じに扱っていて時刻を読むと panic していたので、std の API で実装した |

ライセンスは各ディレクトリの LICENSE のとおり（winit・alacritty_terminal は Apache-2.0、floem は MIT、
filetime と parking_lot_core は MIT OR Apache-2.0）。
