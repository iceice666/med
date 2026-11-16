[English](README.md) | 繁體中文

# med

一個建構在 [GPUI](https://www.gpui.rs/)（Zed 背後的 UI 框架）之上的模式式（vim 風格）程式碼編輯器。

- 自有的編輯核心：具分組復原的 rope 緩衝區（`med-core`），以及不需 UI 就能執行的 vim 引擎（`med-vim`）
- 約 35 種語言的 Tree-sitter 語法高亮，以及依循語法樹的文字物件與移動（函式、型別、引數、註解）
- LSP：診斷、自動完成、懸停說明、簽章提示、跳至定義、參照、重新命名、程式碼動作、格式化；伺服器、其設定與同一檔案的多個伺服器都在 init 檔中設定
- 沒有內建按鍵。vim 按鍵配置、oil.nvim 風格的目錄編輯器、模糊搜尋器、lazygit 風格的 git 畫面和 which-key 都是 [`examples/`](examples) 裡的 Lisp 程式，由你載入、修改或替換

## 安裝

每個 [release](https://github.com/iceice666/med/releases) 都提供 Apple silicon macOS 用的 `.dmg` 與壓縮的 `med.app`，以及 x86_64 和 arm64 Linux 用的 `.deb` 與 `.AppImage`。

macOS 版沒有經過 Apple 開發者簽署，因此 macOS 會說下載來的副本「已損毀」。移到「應用程式」後，清除一次隔離標記即可：

```sh
xattr -dr com.apple.quarantine /Applications/med.app
```

## 建置與執行

flake 提供版本夠新的 rustc 與 rust-analyzer。macOS 需要完整的 Xcode 來使用 Metal 著色器編譯器。

```sh
nix develop            # or: direnv allow
cargo run -p med -- path/to/project
cargo test --workspace
```

`med` 接受一個專案目錄、要開啟的檔案，或什麼都不給（即目前目錄）。`scripts/bundle.sh [debug|release]` 會把執行檔包成 `med.app`。

環境變數：`MED_FONT`、`MED_FONT_SIZE`（預設 14）、`MED_NO_LSP`、`MED_NO_PROJECT_ENV`、`MED_NO_SESSION`、`MED_CONFIG_DIR`（預設為 `$XDG_CONFIG_HOME/med`，否則為 `~/.config/med`）、`MED_STATE_DIR`（預設為 `$XDG_STATE_HOME/med`，否則為 `~/.local/state/med`）。

### Session

沒有指定檔案啟動時（`med` 或 `med dir`），med 會開啟這個專案上次結束時開著的內容：pane 的分割方式、每個 pane 裡的檔案，以及游標所在位置。已經不在磁碟上的檔案會略過，終端機與程式的文字永遠不會保存。指定了檔案啟動時，med 只開那些檔案，結束時也不會動到專案的 session；不論哪種情況，`:mksession`（`:mks`）都會把目前開著的內容存起來。Session 存在 `MED_STATE_DIR` 底下的 `sessions/`，每個專案一個檔案；`MED_NO_SESSION` 可關閉此功能。

每個視窗就是一個專案。`:project dir` 會在新視窗中開啟另一個專案；載入 `picker.mlisp` 後，`space p` 或點擊標題列上的目錄，會列出以前開過的專案。在最後一個分頁上 `:q` 只會關掉那個視窗；`:qa` 則會全部結束。

### 專案環境

語言伺服器（rust-analyzer、clangd、gopls、pyright 等，從專案的 `PATH` 中尋找）以及用 `spawn` 啟動的程式，都在專案的環境中執行。從終端機啟動時，med 沿用它既有的環境；從 Finder、Dock 或桌面啟動器啟動時，則取用你的登入 shell 的環境（`$SHELL -l -i`）。無論哪種情況，只要裝了 direnv，就會再疊上 `direnv export json` 的結果，devbox 與 nix-direnv flake 也因此涵蓋在內。med 從不自行執行 `direnv allow`；允許或編輯 `.envrc` 之後，`:workspace.reload_env` 會重新載入環境並重啟語言伺服器。`MED_NO_PROJECT_ENV` 或 `(set-option 'project-env false)` 可關閉此功能。

## 入門

沒有初始化檔時，除了 `:` 以外沒有任何按鍵有綁定。執行 `:install_default_config` 會把 [`examples/`](examples) 複製到設定目錄並載入（加 `!` 會覆寫已存在的檔案），或者從原始碼目錄載入範例：

```lisp
; ~/.config/med/init.mlisp
(load "/path/to/med/examples/init.mlisp")   ; 全部載入，或挑選檔案：
(load "/path/to/med/examples/vim.mlisp")
(load "/path/to/med/examples/keys.mlisp")
```

| 檔案 | 加入的功能 |
| --- | --- |
| `vim.mlisp` | 各模式的 vim 按鍵，以及 nvim-surround 風格的 `ys` / `ds` / `cs` |
| `keys.mlisp` | LSP 按鍵（`gd`、`gr`、`K`）、分頁、窗格、flash 風格跳躍、`space` leader 鍵、`⌘` 快捷鍵 |
| `oil.mlisp` | 把目錄當成緩衝區來編輯，如同 oil.nvim（`space e`、`space -`） |
| `picker.mlisp` | 模糊檔案搜尋、專案搜尋與最近的專案（`space f`、`space /`、`space p`） |
| `git.mlisp` | lazygit 風格的 git 畫面（`space g`） |
| `which-key.mlisp` | 停頓片刻後，列出接下來可按的鍵 |
| `todo.mlisp` | 在 TODO/FIXME/NOTE 行旁加上標示，`]T` / `[T` |
| `tree.mlisp` | 側邊欄的檔案樹（`space e`、`⌘B`）；`init.mlisp` 不會載入 |
| `dired.mlisp` | Emacs 風格的 dired 與 wdired（`space -`）；`init.mlisp` 不會載入 |
| `statusline.mlisp`、`titlebar.mlisp` | 以可編輯版面呈現的狀態列與標題列 |
| `rss.mlisp` | 訂閱閱讀器（`:newsticker`）；`init.mlisp` 不會載入 |

`:help`（`:h`）會說明任一命令，並列出綁定到它的按鍵。

## 設定

編輯器做的每件事都是一個具名命令，按鍵則在初始化檔（`<config>/init.mlisp`）中或透過 `:eval` 綁定到這些名稱：

```lisp
(bind 'normal "<Space>n" "workspace.next_tab")
(bind 'insert "jk" "insert.escape")
(set-option 'font-size 16)

(defcommand "user.save-and-close" "Write the file, then close its tab"
  (fn ()
    (run "workspace.save")
    (run "workspace.close_tab")))
(bind 'normal "<Space>x" "user.save-and-close")
```

程式也能加入面板、讀取與編輯緩衝區、裝飾行，以及回應 hook。參考文件在 `docs/`：

- [按鍵與命令](docs/keys.zh-TW.md)：預設按鍵、按鍵表、`bind`、`defcommand`、選項、語言伺服器（`lsp-server`、`deflanguage`）
- [範例程式](docs/examples.zh-TW.md)：oil、檔案樹、搜尋器、git 畫面、which-key 與 dired 的詳細說明
- [腳本](docs/lisp.zh-TW.md)：面板、版面、狀態列與標題列、宿主函式、hook、緩衝區 API 以及 Lisp 本身

不開編輯器也能試用這個 Lisp：`cargo run -p med-lisp --example repl`。

## Crate

| Crate | 內容 |
| --- | --- |
| `med-core` | 文字緩衝區、復原、座標轉換 |
| `med-vim` | 模式式編輯引擎 |
| `med-lsp` | 語言伺服器的 JSON-RPC 傳輸層 |
| `med-git` | `git` CLI 的包裝 |
| `med-lisp` | 腳本語言 |
| `med` | GPUI 應用程式 |

## 發布

[release-plz](https://release-plz.dev) 會在 `main` 上維持一個 release PR，依 conventional commits 更新版本號與 `CHANGELOG.md`。合併它會打上 `v{version}` 標籤，接著 `.github/workflows/release.yml` 用 [cargo-packager](https://github.com/crabnebula-dev/cargo-packager) 建置安裝包。在本機打包：

```sh
cargo install cargo-packager --locked
cargo build -p med --release
cargo packager -p med --release --formats app,dmg --out-dir "$PWD/dist"        # macOS
cargo packager -p med --release --formats deb,appimage --out-dir "$PWD/dist"   # Linux
```

相對路徑的 `--out-dir` 以 `crates/med` 為基準。要重建某個 release 的安裝包，請以它的標籤手動執行該 workflow。

## 授權

MIT。請見 [LICENSE](LICENSE)。
