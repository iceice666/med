[English](keys.md) | 繁體中文

# 按鍵與命令

## 按鍵

med 本身不綁定任何按鍵：沒有初始化檔時，除了 `:` 之外沒有任何按鍵有作用；只要 normal 模式中完全沒有綁定，`:` 就會開啟命令列。編輯器上方會出現一個彈出視窗說明這件事，以及要輸入什麼；命令列開啟時它會暫時讓開，按 escape 或點擊視窗外即可關閉，綁定按鍵後也會自動消失。在命令列中，`:install_default_config` 會把 [`examples/`](../examples) 的每個檔案寫入設定目錄，並執行其中的 `init.mlisp`；已經存在的檔案會保留不動，並列出是哪些，`:install_default_config!` 則會覆寫它們。

以下按鍵出自 [`examples/vim.mlisp`](../examples/vim.mlisp)（vim 的部分）與 [`examples/keys.mlisp`](../examples/keys.mlisp)（其餘部分），由初始化檔依序載入：

```lisp
; ~/.config/med/init.mlisp
(load "/path/to/med/examples/vim.mlisp")
(load "/path/to/med/examples/keys.mlisp")
```

編輯方式就是 vim：模式（`v`、`V` 與 `ctrl-v` 的區塊模式，區塊中每一行都可用 `I`、`A` 和 `c`）、次數、運算子（`d c y > < gu gU g~ gc`）、移動、文字物件（`iw i" i( i{ ip` …）、暫存器、標記（`ma`、`'a`、`` `a ``、`''`）、巨集（`qa` … `q`、`@a`、`@@`）、`.`、`/ ? n N * #`、`:s`、`:w :q :wq :e :bn :bp :bd :wa :qa :mks :project :vs :sp :close :only`。無名暫存器就是系統剪貼簿。搜尋如同 vim 開了 `ignorecase` 與 `smartcase`：pattern 中沒有大寫字母時不分大小寫，有大寫字母時只比對原本的大小寫，也可以在 pattern 中用 `\c` / `\C` 指定；`*` 和 `#` 則不分大小寫地比對游標下的字。

| 按鍵 | 動作 |
| --- | --- |
| `gd` / `gr` / `K` | 定義 / 參照 / 懸停說明 |
| `]d` / `[d` | 下一個 / 上一個診斷 |
| `space r` | 重新命名：在 `:rename` 之後輸入新名稱 |
| `space a` | 針對游標或選取範圍的程式碼動作：`j` `k`、`enter` 或數字 |
| `space =`、`:format` | 格式化檔案 |
| `ctrl-s`（insert） | 目前呼叫對象的簽章；輸入 `(` 之後會自動出現 |
| `ctrl-n` `ctrl-p` `tab` | 自動完成選單：下一個、上一個、接受 |
| `tab` / `shift-tab`（在 `:` 中） | 補完命令名稱、`:e` `:w` 之後的檔案，或 `:h` 之後的主題；再按一次則切換到下一個 / 上一個符合項 |
| `ctrl-o` / `ctrl-i` | 跳回 / 跳往 |
| `gt` / `gT`、`⌘⇧]` / `⌘⇧[` | 下一個 / 上一個分頁 |
| `ctrl-w v` / `ctrl-w s`、`:vs` / `:sp` | 分割窗格：在右側 / 下方開一個新窗格，內含空緩衝區 |
| `ctrl-w h j k l`、`ctrl-w w` / `ctrl-w W` | 左、下、上、右的窗格；下一個 / 上一個窗格 |
| `ctrl-w c` / `:close`、`ctrl-w o` / `:only` | 關閉此窗格 / 其他所有窗格：它們的分頁會移到保有鍵盤焦點的窗格 |
| `space w` / `⌘S`、`space q` / `⌘W` | 存檔、關閉分頁 |
| `space m`、`:preview` | 在右側分割出的窗格中渲染 Markdown 檔案，並隨輸入即時更新，鍵盤焦點留在原檔；再按一次則在預覽與原檔之間切換 |
| `space t`、`:terminal` | 在下方分割出的窗格中開啟 shell，工作目錄為專案根目錄並使用專案環境；再按一次則回到它（若仍在執行）。`:terminal cmd` 在新的 terminal 中執行 `cmd`，`:terminal!` 另開一個 shell。在 terminal 中除了 ⌘ 組合鍵，所有按鍵都交給程式；滑鼠滾輪與 `shift-pageup` / `shift-pagedown` 可往回捲動先前的輸出，用滑鼠拖曳可選取文字（雙擊選詞、三擊選行），`⌘C` 複製。`ctrl-\ ctrl-n` 進入 normal mode：把先前的輸出複製到一個編輯器中，可移動、搜尋、yank，也可從這裡切到其他窗格；這份副本不會即時更新，對它的修改也不會保留。`i`、`a` 或其他進入插入模式的鍵會回到程式。程式結束後，按任意鍵關閉該分頁，仍可用 normal mode 閱讀它的輸出 |

環繞，如同 nvim-surround：`ys` 加上一個移動或文字物件，再加上分隔符號（`ysiw)`；整行用 `yss"`），`ds"` 移除一組，`cs"'` 替換它，選取範圍上則用 `S`。閉括號，或 `b` `r` `B` `a`，代表原樣的那一對；開括號則會在內側兩邊各加一個空白。

要跳到畫面上的文字，如同 flash.nvim：按 `s`，再輸入一部分文字。每個含有該文字的位置都會被標記並分配一個字母，越近的位置分到越好按的字母，其餘文字則變暗。按下字母就跳到它的位置，`enter` 跳到游標之後的第一個位置；backspace 刪回一個字元，escape 放棄，輸入畫面上哪裡都沒有的文字也會放棄。可能成為文字後續的字母絕不會被當作標籤；在輸入大寫字母之前，小寫字母也會比對大寫。在 visual 模式中，選取範圍會延伸到那裡。在運算子之後要用 `z`，因為 `s` 是環繞的鍵，而範圍會包含找到位置的第一個字元：`dzfoa`。這個命令是 `motion.flash`；vim 原本的 `s`，仍可用 `cl` 達成。

flash.nvim 的其他功能也使用同樣的標記：

| 按鍵 | 動作 |
| --- | --- |
| `f` `F` `t` `T` | 在畫面上的每一行搜尋，而不只是游標所在行。其他可去的位置會被標記；標記還在時，再按同一個鍵前往下一個（如同 `;`），`f` 之後按 `F` 則往回（如同 `,`）；其他任何鍵則恢復它原本的作用 |
| `S` | 在游標周圍每個語法節點的前後放上標籤，最小的節點是 `a`。按標籤選取它的節點，`enter` 選取最小的；`]x` 與 `[x` 從那裡繼續。在運算子之後，該節點就是運算子作用的對象：`dSs` |
| 運算子之後的 `r` | 讓運算子到別處作用的 `s`：輸入文字和它的標籤，再在那裡輸入一個移動或文字物件，然後游標回到原處。`yrfoaiw` 會複製下方較遠處的一個詞 |

這些命令是 `motion.flash_find_forward` 及其他三個同類命令、`select.flash` 和 `motion.flash_remote`。vim 原本只在當行作用的 `f` 和 `t` 是 `motion.find_forward` 等命令；原本的 `S` 仍可用 `cc` 達成。

括號與引號在輸入時會自動補上另一半，輸入閉合的那一半時會直接跳過，在兩者之間按 backspace 會一起刪除；`(set-option 'auto-pairs false)` 可關閉此功能。

`Tab`、`>>` 與新行的縮排寬度依檔案的語言而定（med-lisp、Markdown、TOML、Nix、JavaScript 等為 2，Rust、Python 為 4），或依檔案既有的縮排而定。`(set-option 'tab-width 4)` 讓每個檔案都縮排 4 欄；設為 `nil` 則恢復原樣。以 tab 縮排的檔案仍使用 tab，顯示為該寬度。

`(set-option 'relative-number true)` 讓每一行的行號顯示它與游標所在行的距離，也就是用 `j` 或 `k` 到達那一行所需的次數，如同 vim 同時開啟 `relativenumber` 與 `number`：游標所在行仍顯示自己的行號。

畫面會在游標與上下邊緣之間保留 3 行，如同 vim 的 `scrolloff`；`H`、`L`、`ctrl-e` 與 `ctrl-y` 也遵守這個距離，只有在檔案開頭與結尾例外。`(set-option 'scroll-off 5)` 改成 5 行；設為 0 則游標可到達邊緣，設為 999 則讓游標保持在中間。

游標所在行會被標示出來，如同 vim 的 `cursorline`；`(set-option 'cursor-line false)` 讓它和其他行一樣。`(set-option 'color-column 80)` 會在畫面中標示第 80 欄，如同 vim 的 `colorcolumn`；給一個清單如 `'(80 120)` 會標示每一欄，設為 `nil` 則不標示，這也是預設值。

`(set-option 'indent-guides true)` 會在每一層縮排畫一條細線（每層的寬度為檔案的 tab 寬度），如同 vim 的 indent-blankline；空白行沿用前後行的縮排，讓區塊的線條能連續穿過。游標所在區塊的那條線會畫得較深，貫穿整個區塊。也可以用 `set-buffer-option` 只對單一分頁設定。

`(set-option 'sticky-scroll true)` 會把畫面頂端所在的函式、型別與模組的第一行固定在畫面頂端，一層疊一層，如同 VS Code 的 sticky scroll；在 markdown 中則是各段落的標題。它依語法樹而定，沒有語法的檔案不會顯示；閉包不算在內。當某個範圍的結尾捲上來時，它的那一行會被推出畫面。最多佔畫面的三分之一、至多五行，且不會蓋住游標所在行。點一下會跳到該行。也可以用 `set-buffer-option` 只對單一分頁設定。

行號旁邊會用一條線標出與 HEAD（最後一次 commit）不同的行：綠色是新增的行，藍色是修改的行，紅色的短橫線則表示那裡有行被刪除。它會在打字停下片刻後跟著文字更新。已 stage 的變更仍然是相對於 HEAD 的變更，在 commit 之前都會保留標記。視窗回到前景時，以及執行 `workspace.sync` 之後（git 畫面每做完一件事都會執行它），會重新讀取 HEAD。git 沒有追蹤的檔案，或不在任何 repository 裡的檔案，不會有標記。`(set-option 'git-gutter false)` 可以關閉；也可以只對單一分頁設定。

`(set-option 'inlay-hints true)` 會在行內顯示語言伺服器提供的提示，以較淡的顏色顯示在對應的位置：例如變數的型別、引數所對應的參數名稱。文字停止變動片刻後才會向伺服器取得提示，伺服器表示有新提示時也會重新取得；在那之前，提示會隨著編輯的文字移動。換列時不顯示。也可以用 `set-buffer-option` 只對單一分頁設定。

`(set-option 'theme "Catppuccin Mocha")` 讓視窗使用該主題的顏色（包括文字與語法上色），不論系統外觀為何；`'("Catppuccin Latte" "Catppuccin Mocha")` 指定一個淺色與一個深色主題，依系統外觀切換；設為 `nil` 則回到 med 內建的淺色與深色主題。主題來自設定目錄下 `themes/` 中的檔案，採用 gpui-component 的主題格式，在啟動時讀取，檔案變更時也會重新讀取。`examples/themes/catppuccin.json` 包含 Catppuccin 的 Latte 與 Mocha，依其調色盤製作（<https://github.com/catppuccin/palette>，MIT 授權），`:install_default_config` 會把它寫到該處。

`(set-option 'undo-file true)` 會在存檔時保留每個檔案的復原紀錄，如同 vim 的 `undofile`；之後再開啟該檔案（不論在這次或之後的執行中）都會接續這份紀錄：`u` 可以越過存檔點往回復原，最多到最近的 1000 個變更。紀錄存放在 med 的狀態目錄（`$MED_STATE_DIR`，否則為 `$XDG_STATE_HOME/med`，再否則為 `~/.local/state/med`）下的 `undo/`。檔案若在之後被其他程式修改過，開啟時就沒有紀錄。這個選項適用於所有分頁，在開啟與儲存檔案時讀取。

`(set-option 'wrap true)` 讓比畫面寬的行接續到下一列顯示，而不是超出右邊緣，如同 vim 的 `wrap`：可以在任何字元處換列，寬字元或 tab 則整個移到下一列。只有第一列會顯示行號。畫面以列為單位捲動，`scroll-off` 也以列計算，點擊任何一列都會落在該處的字元上。`j` 和 `k` 仍然一次移動一行，`gj` 和 `gk`（`motion.row_down`、`motion.row_up`）則一次移動一列，並停在列中相同的格位；沒有換列時它們就等於 `j` 和 `k`。行尾之後的文字（診斷訊息或程式加上的文字）會在右邊緣被截斷，`color-column` 則標示畫面上的欄位，每一列都會標示。搭配 `set-buffer-option` 與 `buffer-language` 可以只對部分檔案開啟：在語言為 `"markdown"` 時使用 `(set-buffer-option 'wrap true)`。

檔案有語法樹時：

| 按鍵 | 動作 |
| --- | --- |
| `if` `af`、`it` `at` | 函式、型別：其本體，或整個 |
| `ia` `aa` | 一個引數或參數，連同它的逗號 |
| `ic` `ac` | 註解，連續的多行視為一個 |
| `]f` `[f`、`]t` `[t`、`]a` `[a`、`]c` `[c` | 上述各項的下一個 / 上一個 |
| `]x` / `[x` | 選取游標周圍的節點，再選取包住它的節點；以及反向 |


## 命令與按鍵綁定

沒有任何按鍵直接連到某個行為。編輯器能做的每件事都是一個具名命令，按鍵則在按鍵表中綁定到這些名稱：

- vim 引擎的命令（`motion.word_forward`、`operator.delete`、`insert.newline`…，見 `med_vim::command_names()`）。次數、暫存器以及運算子加移動屬於語法，不論綁定的是哪些按鍵都能運作。它的標準按鍵配置是 `examples/vim.mlisp`；`med-vim/src/keymap.rs` 中的 `Keymap::vim()` 是同一張表，為測試而保留。
- 應用程式的命令（`workspace.*`、`editor.*`），由各個 view 在 `Commands` 中註冊，以及程式定義的命令（`tree.*`、`git.*` 和 `picker.*` 屬於範例）。
- `examples/keys.mlisp`：作用超出緩衝區的編輯器按鍵（`gd`、`space` leader 鍵）、flash 的按鍵與 `⌘` 快捷鍵。
- `med/src/defaults.rs`：ex 命令對應的名稱（`:w` 即 `workspace.save`），這是唯一內建的表，以及 `:install_default_config` 寫出的 `examples/` 副本。

按鍵以 vim 記法書寫（`<C-w>`、`<Space>f`、`<CR>`）。任何命令也都能用全名執行，例如 `:workspace.find_file`。

`:help`（`:h`）說明命令的作用：`:help w` 或 `:help workspace.save` 會列出命令的自我描述、命令列認得它的各個名稱，以及綁定到它的每個按鍵，涵蓋各模式的表、緩衝區的表、面板的表與 `app`。其中沒有任何內容是手寫的：頁面在被要求時才產生，所以反映的是當下初始化檔中的按鍵與命令。單獨輸入 `:help` 會列出所有命令，vim 引擎的命令則附上它們在 normal 模式中的按鍵。這些頁面會出現在一個名為 `help` 的專屬分頁中，每次 `:help` 都會重複使用它。

按鍵表可在啟動時執行的初始化檔 `<config>/init.mlisp` 中修改，或在命令列用 `:eval` 修改：

```lisp
(bind 'normal "<Space>n" "workspace.next_tab")
(bind 'insert "jk" "insert.escape")
(bind '(normal visual) "gh" "editor.hover")   ; 一次多個模式
(bind-keys 'normal "Y" "y$")                  ; 按下時改為輸入其他按鍵
(bind 'git.files "x" "git.discard")           ; 面板的按鍵
(bind 'app "cmd-shift-t" "tree.toggle")       ; 視窗的快捷鍵
(unbind 'normal "gt")
(set-option 'font-size 16)                    ; 另有 font-family、lsp、key-timeout、
                                              ; auto-pairs、format-on-save、tab-width、
                                              ; relative-number、scroll-off、cursor-line、
                                              ; color-column、wrap、inlay-hints、
                                              ; indent-guides、sticky-scroll、
                                              ; git-gutter、undo-file、theme、
                                              ; project-env
```

模式有 `normal`、`visual`、`operator` 和 `insert`。面板的表以面板名稱命名，或以名稱加上面板的某個部分命名（`git.files`）。`app` 是不論鍵盤焦點在哪裡都有效的快捷鍵表，以視窗系統的寫法書寫（`cmd-shift-p`）。`bind` 會拒絕不存在的命令。

分頁可以有自己的按鍵，放在程式用 `defkeys` 命名的表中。分頁會先在自己的表中查找按鍵，這些表都不認得的鍵，再到它所在模式的表中查找：

```lisp
(defkeys 'listing)
(bind 'listing "<CR>" "user.open")          ; normal 模式中
(bind 'listing.insert "<C-j>" "user.next")  ; 另有 .visual、.operator
(set-buffer-keys '(listing))                ; 目前顯示的分頁使用它，nil 表示不用
(buffer-keys)                               ; => ("listing")
```

分頁的表所綁定的鍵會立即執行，即使模式的表中有以它開頭的較長綁定。`:workspace.reload_init` 會重新執行該檔案；檔案中的錯誤會連同行號顯示在狀態列。

分頁也可以有自己的選項值：`tab-width`、`relative-number`、`format-on-save`、`auto-pairs`、`scroll-off`、`cursor-line`、`color-column`、`wrap`、`inlay-hints`、`indent-guides`、`sticky-scroll` 與 `git-gutter`（其他選項屬於視窗或整個應用程式）。分頁自己的值優先於 `set-option` 給所有分頁的值，直到被重設為止。搭配每個檔案出現時都會通知的 `buffer-entered`，以及 `buffer-language`，選項就能依語言而定：

```lisp
(set-buffer-option 'tab-width 8)            ; 目前顯示的分頁自己的值
(reset-buffer-option 'tab-width)            ; 回到 set-option 的值
(buffer-language)                           ; => "markdown"，沒有語言時為 "text"

(add-hook 'buffer-entered
  (fn (path)
    (when (= (buffer-language) "sql")
      (set-buffer-option 'format-on-save false))))
```

命令也可以寫在那裡。命令是一個不帶引數的函式，從此就是一個和其他名稱無異的名稱：可用於按鍵、命令列，以及 `run`，後者以命令列認得的任何名稱執行命令。

```lisp
(defcommand "user.save-and-close" "Write the file, then close its tab"
  (fn ()
    (run "workspace.save")
    (run "workspace.close_tab")))
(bind 'normal "<Space>x" "user.save-and-close")

(defcommand "user.note" "Open the note named on the command line"
  (fn () (run "workspace.edit" (str "notes/" command-arg ".md"))))
```

`:user.note today` 會開啟 `notes/today.md`。除了代表命令列其餘部分的 `command-arg`，還有 `command-bang`（是否有 `!`）和 `command-count`（按鍵前輸入的次數，或 `nil`）。`(current-file)` 和 `(project-root)` 告訴你編輯器目前的位置。命令不能使用編輯器內建命令的名稱。

一個本身就有綁定、同時也是較長綁定開頭的按鍵（例如會向右移動的 `space`），會等待一秒讓你輸入其餘部分，然後才執行。`(set-option 'key-timeout 300)` 可設定另一個等待時間，單位為毫秒；設為 `nil` 則會一直等到下一個按鍵，不論多久。

## 語言伺服器

檔案由其語言的語言伺服器負責，只要能在專案的 `PATH` 中找到。med 為它認得的語言內建了伺服器（`rust-analyzer`、`clangd`、`basedpyright-langserver`，找不到時依序是 `pyright-langserver`、`pylsp`，以及 `typescript-language-server`、`gopls` 等），各以其程式名稱稱呼；`(lsp-servers)` 會全部列出。init 檔可以修改、關閉它們，也可以加入其他伺服器：

```lisp
; med 內建的：只改給出的部分，其餘不變。
(lsp-server "rust-analyzer"
  :settings '(:rust-analyzer (:check (:command "clippy") :cargo (:features "all"))))
(lsp-server "pylsp" :enabled false)

; 自己加的需要指令與它負責的語言。
(lsp-server "eslint"
  :command "vscode-eslint-language-server" :args '("--stdio")
  :languages '("javascript" "typescript" "tsx")
  :settings "{\"validate\": \"on\", \"run\": \"onType\", \"quiet\": false,
              \"useESLintClass\": false, \"experimental\": {}, \"nodePath\": \"\",
              \"problems\": {\"shortenToSingleLine\": false},
              \"rulesCustomizations\": [], \"onIgnoredFiles\": \"off\",
              \"codeActionOnSave\": {\"enable\": false, \"mode\": \"all\"},
              \"codeAction\": {\"disableRuleComment\": {\"enable\": true, \"location\": \"separateLine\"},
                             \"showDocumentation\": {\"enable\": true}},
              \"workingDirectory\": {\"mode\": \"auto\"}}")
```

有些伺服器（例如 eslint）不會自行補上沒給的設定：它們的設定要寫齊文件（或 nvim-lspconfig 對它們的設定）列出的所有項目。

`:settings` 是伺服器詢問設定時（`workspace/configuration`，一次一個 section，例如 `"python.analysis"`）得到的回答，伺服器啟動後也會送給它一次；`:init-options` 隨 `initialize` 送出。兩者都可以寫成 keyword 與值交錯的串列，`'(:a 1 :b (:c true))` 即 `{"a": 1, "b": {"c": true}}`，其他串列則是陣列；也可以直接寫成 JSON 字串，照抄伺服器文件上的寫法。`:enabled false` 讓伺服器不會啟動。

med 不認得的語言用 `deflanguage` 命名，讓這些檔案在 `buffer-language` 與伺服器眼中有了語言，但沒有語法高亮：

```lisp
(deflanguage "dart" :extensions '("dart") :comment "//" :tab-width 2)
(lsp-server "dartls" :command "dart" :args '("language-server" "--protocol=lsp")
  :languages '("dart"))
```

選項有 `:extensions`、`:filenames`（`"Justfile"`）、`:lsp-id`（告訴伺服器的 `languageId`，預設為名稱）、`:comment`、`:tab-width` 與 `:expand-tab`。若以 med 已有的語言命名，則是為該語言增加檔案，並保留其語法高亮。

檔案語言的每個伺服器都會負責它，順序依設定的先後；但 med 內建、彼此可替代的伺服器（例如 Python 的三個）只取第一個找到的。所有伺服器都會收到檔案的內容，所有伺服器的診斷、code action、補全與 hover 都會顯示，hover 依伺服器順序上下分開列出；code action 由提供它的伺服器執行。補全會問每個提供補全的伺服器，但打出觸發字元時只問登記了該字元的伺服器；比對分數相同時，排在前面的伺服器的項目先列出。其他功能（定義、參照、重新命名、signature help、inlay hint、格式化）交給第一個表示能做的伺服器。

伺服器會沿用啟動時的設定：修改 init 檔並重新執行（`:workspace.reload_init`）之後，`:workspace.reload_env` 會以新的設定重新啟動伺服器。
