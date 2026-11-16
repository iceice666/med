[English](lisp.md) | 繁體中文

# 腳本

## 面板

程式可以加入面板：編輯器左右兩側的側邊欄、視窗底部的長條、浮在編輯器上的彈出視窗，以及取代編輯器的整個畫面。

```lisp
(def notes '("call Ada" "ship it"))

(defpanel 'notes :at 'right :width 240
  (fn (panel)
    (col :width 'full :height 'full :bg 'sidebar
      (text "Notes" :bold true :padding-x 12 :height 32)
      (rows notes :grow true
        (fn (note)
          (row :width 'full :height 24 :padding-x 12
               :hover (list :bg 'list-hover)
               :on-click (fn () (set! notes (filter (fn (n) (not (= n note))) notes)))
            (text note)))))))

(defcommand "user.notes" "Show or hide my notes" (fn () (toggle-panel 'notes)))
(bind 'normal "<Space>n" "user.notes")
```

`defpanel` 接受一個名稱、選項 `:at`（`'left`、`'right`、`'bottom`、`'popup`、`'full`，或用於[狀態列與標題列](#狀態列與標題列)的 `'status` 和 `'title`）、`:width`、`:shown`（一開始就顯示）和 `:contexts`（見下文），以及一個版面。`show-panel`、`hide-panel` 和 `toggle-panel` 顯示與收起面板，`panel-shown?` 則查詢它是否顯示中。彈出視窗的高度等於其內容的高度，點擊它旁邊或按 escape 會將它收起。底部面板的高度同樣等於其內容的高度，橫跨整個視窗、位於狀態列上方，並在 `'full` 面板之下仍然保留；它不會搶走上方內容的按鍵與點擊，因此適合用來顯示輸入命令時的資訊。`set-layout` 可替已定義的面板換上另一個版面。

### 狀態列與標題列

狀態列與標題列由視窗自己繪製，直到有程式為其中之一提供版面：位於 `'status` 的面板會取代狀態列繪製，位於 `'title` 的面板則畫在標題列中；標題列仍可拖曳來移動視窗、雙擊來縮放，在 macOS 上則從視窗按鈕之後開始。這樣的面板一經定義就會顯示；`hide-panel` 會讓視窗恢復自己的列，版面出錯時也會如此（狀態列會提示錯誤），直到該面板再次被賦予版面。同時有多個顯示時，繪製最後定義的那一個。

```lisp
(defpanel 'mine :at 'status
  (fn (panel)
    (row :width 'full :height 24 :padding-x 8 :gap 12 :align 'center :bg 'status-bar
      (text (or (get panel :cmdline) (get panel :path) "") :grow true)
      (text (str (get panel :line) ":" (get panel :col))))))
```

除了每個面板都有的內容之外，列的 `panel` 還包含視窗的列會顯示的資訊，沒有東西可顯示時為 `nil`：

| 名稱 | 內容 |
| --- | --- |
| `:title` | 視窗標題，例如 `"src/main.rs — med"` |
| `:mode`、`:mode-label` | 模式，分別是 `(mode)` 給出的形式與狀態列顯示的名稱：`'normal`、`"NORMAL"` |
| `:path`、`:scratch` | 正在編輯的檔案（相對於專案根目錄）；非檔案分頁的名稱 |
| `:modified` | 是否有尚未寫入的變更 |
| `:line`、`:col` | 游標位置，從 1 開始計算 |
| `:language` | `"rust"`，或 `"text"` |
| `:cmdline` | 輸入中的命令列，例如 `":wq"` 或 `"/foo"` |
| `:cmdline-before`、`:cmdline-after` | 命令列游標前後的文字，例如 `":w"` 和 `"q"`；兩者合起來就是 `:cmdline` |
| `:cmdline-preedit` | 輸入法正在游標處組字、尚未送出的文字 |
| `:message` | 最後一則訊息，直到下一次按鍵 |
| `:diagnostic` | 游標處診斷的第一行 |
| `:pending` | 目前已輸入的命令按鍵，例如 `"2d"` |
| `:recording` | 正在錄製巨集的暫存器 |
| `:errors`、`:warnings` | 所有檔案中各類診斷的數量 |
| `:lsp` | 檔案的語言伺服器正在做什麼 |
| `:branch` | 專案目前所在的分支 |

沒有開啟任何檔案時，會有一個空的 `[No Name]` 緩衝區，下一個開啟的檔案會取代它：它有 `:mode`，但沒有任何來自檔案的資訊。省略 `:cmdline` 的狀態列會讓 `:` 的輸入看不見。要畫出命令列的游標，把 `(caret)` 放在 `:cmdline-before` 與其餘部分之間：它沒有寬度，但輸入法會把候選視窗放在它所在的位置；沒有 `(caret)` 時，候選視窗會在列的角落。範例見英文版文件或 [`examples/statusline.mlisp`](../examples/statusline.mlisp)。[`examples/statusline.mlisp`](../examples/statusline.mlisp) 就是視窗原本的狀態列，各列的顏色是 `'status-bar`、`'title-bar`、`'title-bar-border` 和 `'tab-bar`。

### 版面

面板的外觀就是它的版面：一個在每次繪製面板時被呼叫、用來描述面板的函式。

| 函式 | 產生 |
| --- | --- |
| `(row styles... children...)`、`(col ...)` | 子元素排成的一列或一欄 |
| `(text value styles...)` | 文字 |
| `(rows list styles... function)` | 可捲動的清單；函式描述每一列 |
| `(input name styles...)` | 一行輸入框 |
| `(markdown value styles...)` | 以 Markdown 解讀的文字：標題、清單、程式碼、表格、連結 |
| `(caret styles...)` | 列畫出命令列游標的位置，供輸入法使用 |

樣式是 `:name value` 成對的值：`width` 和 `height`（像素或 `'full`）、`grow`（`true`，或與其他同樣會伸展的元素分配剩餘空間時的比例）、`shrink`、`padding-x` `padding-y` `padding-left` `padding-right`、`gap`、`align` 和 `justify`（`'start` `'center` `'end`，以及 `'between`）、`bg`、`color`、`border` `border-top` `border-right` `border-bottom` `border-left`、`border-color`、`rounded`、`size`（文字大小）、`bold`、`underline`、`mono`、`nowrap`、`clip`；`hover`，滑鼠懸停時套用的樣式清單，例如 `(list :bg 'list-hover)`；以及 `on-click`，一個命令名稱或函式。顏色可以是主題中的顏色（`'sidebar`、`'muted-foreground`、`'list-active`、`'warning`…）或 `"#rrggbb"`。

值為 `nil` 的樣式會被忽略，`nil` 子元素也等於沒有子元素，所以依值而定的內容可以用 `if`、`when` 和 `cond` 撰寫。子元素也可以是字串，或像 `map` 產生的子元素清單。

版面的 `panel` 包含 `:name`、`:file`（正在編輯的檔案，或 `nil`）、`:root` 和 `:focused`。面板顯示的其他任何內容都屬於程式自己：用 `def` 保存、用 `set!` 修改的值。每當任何程式執行完畢，面板都會重繪，所以點擊後改變的值會立即顯示。

`rows` 只繪製可見範圍內的列，而這些列必須等高。它的函式會以清單中的一個值被呼叫；若函式接受第二個引數，還會傳入該值在清單中的位置。`:reveal` 指定要保持在可見範圍內的列，`:top` 指定要位於頂端的列：兩者任一改變時，清單就會捲動。

`input` 自行保存它的文字。`:placeholder` 在內容為空時顯示，`:on-change` 在每次輸入時以文字被呼叫，`:on-submit` 則在按下 enter 時被呼叫。`(input-text panel name)` 讀取文字，`(set-input panel name text)` 寫入文字。

`markdown` 的高度與它顯示的內容相同，除非給它 `:scroll true`：此時它的高度由樣式決定（`:grow true` 或 `height`），放不下的部分可以捲動。

版面只負責描述：繪製期間它不能綁定按鍵或執行命令，不過它交給 `on-click` 和輸入框的函式在被呼叫時可以。出錯的版面會以其錯誤訊息的形式繪製。

### 鍵盤

`(focus-panel name)` 顯示面板並把鍵盤交給它，若面板有輸入框則交給輸入框；`(panel-focused? name)` 查詢面板是否持有鍵盤，`(focus-editor)` 則把鍵盤交還給編輯器。點擊面板不會改變鍵盤所在的位置。持有鍵盤的面板被收起時，鍵盤會交還給它原本的來處。

持有鍵盤的面板會在以自己名稱命名的表中查找按鍵：

```lisp
(defcommand "user.close-notes" "Put my notes away" (fn () (hide-panel 'notes)))
(bind 'notes "q" "user.close-notes")
```

`:contexts` 指定其他的表，最具體的放在最前面：可以是一個清單，或回傳清單的函式，適用於按鍵依顯示內容而定的面板。git 畫面的是 `(fn () (list (str "git." git-section) "git"))`。沒有任何表認得的按鍵會交給面板的輸入框。

### 程式可以查詢的資訊

需要時間的操作會接受一個函式，用答案呼叫它，並立即回傳。

| 函式 | 給出 |
| --- | --- |
| `(current-file)`、`(project-root)` | 編輯器目前的位置 |
| `(recent-projects)` | 開過視窗的目錄，最近的在前，只列出仍存在的 |
| `(open-project dir)` | 叫出某個目錄的視窗：已開在該目錄上的那個，或開一個新的 |
| `(buffer-language)` | 目前顯示的分頁的語言，名稱與狀態列相同：`"rust"`，沒有語言時為 `"text"` |
| `(open-file path [line [column]])` | 在分頁中開啟檔案，並跳到某個位置（從 1 開始計算） |
| `(list-dir path [all])` | 目錄的內容：`((:name :path :dir)...)`，除非指定 `all`，否則不含 git 忽略的項目 |
| `(path-name p)`、`(path-parent p)`、`(path-join a b...)` | 路徑的各部分 |
| `(file-exists? path)`、`(dir? path)` | 該處是否存在任何東西；是否為目錄 |
| `(file-info path)` | 該處的資訊，若為連結則是連結本身而非其指向的目標：`(:name :path :kind :dir :size :modified :mode :links :uid :gid :target)`，或 `nil` |
| `(regex-match pattern text)` | 第一個符合項及各群組的符合內容，或 `nil` |
| `(regex-replace pattern text replacement)` | 取代每個符合項，`$1` 與 `${name}` 代表群組 |
| `(spawn program arguments dir function)` | 在 `dir`（`nil` 表示專案目錄）中、以專案環境執行程式；函式會收到 `(:status :out :err)` 和 `nil`，或 `nil` 和無法執行的原因 |
| `(walk-files function)` | 專案的每個檔案，以相對於根目錄的路徑表示 |
| `(fuzzy query list limit)` | 幾個字母所能代表的文字，最佳者優先 |
| `(grep query limit function)` | 符合的行：`((:path :file :line :col :text)...)` |
| `(git what arguments... function)` | 存放庫：見 `crates/med/src/host.rs` |
| `(bindings table [keys])` | 在模式或面板的表中，可接在 `keys` 之後的按鍵 |
| `(after milliseconds function)` | 在那麼多毫秒後呼叫，並回傳一個計時器，可用 `(cancel timer)` 取消 |
| `(add-hook name function)` | 某事發生時呼叫 |

程式也能更改磁碟上的內容。以下每個函式完成後回傳 `nil`，否則回傳出錯的原因；不是從根目錄開始的路徑以專案為基準：

| 函式 | 作用 |
| --- | --- |
| `(rename-file from to)` | 移動檔案或目錄，絕不覆蓋其他檔案；開著它的分頁會一起跟過去 |
| `(copy-file from to [function])` | 複製檔案，或連同所有內容複製目錄，連結以連結形式複製，絕不覆蓋其他檔案；給了函式時在背景執行，函式會收到 `copy-file` 本來會回傳的值 |
| `(delete-file path [forever])` | 把檔案或目錄移到垃圾桶，或永久刪除 |
| `(make-dir path)` | 建立目錄，以及它所在的各層目錄 |
| `(make-file path)` | 建立空檔案，絕不覆蓋已存在的東西 |
| `(make-link target path [hard])` | 在 `path` 建立指向 `target`（照原樣寫入）的符號連結，或硬連結 |
| `(set-file-mode path mode)` | 權限，如同 chmod：`493` 或 `"755"` |

每個視窗就是一個專案。`:project dir`（`workspace.open_project`）會在新視窗中開啟另一個專案；如果已經有視窗開在那個目錄上，就把它叫到前面。在最後一個分頁上 `:q`，或關閉視窗，都只會關掉那一個視窗；`:qa` 則會結束所有視窗，並保存每個視窗的 session。哪些 panel 開著由各個視窗各自決定；init 檔開啟的 panel 會出現在每個視窗。開過的專案列在 `MED_STATE_DIR` 底下的 `projects`。所有視窗共用同一個直譯器，所以程式中的名稱在每個視窗裡都相同：要保存某個專案的資料，應以 `(project-root)` 區分，就像 `tree.mlisp` 和 `picker.mlisp` 那樣。命令、hook 與點擊都在它們來自的視窗中執行；layout 則從 `:root` 得知所屬視窗的專案。

hook 有 `opened`（視窗已開啟，且有專案）、`files-changed`（有檔案被儲存、視窗回到前景，或執行了 `workspace.sync`）、`locations`（一份可供選擇的位置清單，例如某個名稱的參照）、`keys-pending`（見下文）、`terminal-exited`（見 Terminal 一節），以及與正在編輯的檔案相關的 hook（見後文）。

### 正在輸入的按鍵

程式可以追蹤正在輸入的命令，以顯示接下來可以按什麼。每當目前已輸入的按鍵所等待的東西改變時，`keys-pending` hook 就會被呼叫：當它們不再等待任何東西時以 `nil` 呼叫，在那之前則以下列內容呼叫

| 名稱 | 內容 |
| --- | --- |
| `:tables` | 下一個按鍵會在其中查找的按鍵表，第一個認得該鍵的表決定結果：分頁自己的表，然後是它所在模式的表，例如 `("normal")` 或 `("listing.insert" "insert")`，或面板的表，例如 `("git.files" "git")` |
| `:keys` | 目前已輸入的綁定開頭，例如 `"<Space>"`；在次數或運算子之後為 `""`，此時表中任何綁定都可以接著輸入 |
| `:typed` | 命令目前為止的每個按鍵，例如 `"2dg"` |
| `:awaiting` | 當下一個按鍵不經查找而照原樣取用時（在 `"`、`f` 或 `r`、`di`、`m`、`s` 之後），為 `'register`、`'char`、`'text-object`、`'mark` 或 `'flash` |

`(bindings table keys)` 依按鍵記法的順序列出可以接續的內容，每項為 `(:key :keys :command :description :to :prefix)`：該按鍵（`"f"`）與包含它的完整按鍵（`"<Space>f"`）、它們綁定的命令及其自我描述（vim 引擎自己的命令只有名稱，所以為 `nil`），或者它們所代表的按鍵（`:to`），以及是否有更長的綁定從那裡繼續。不給 keys 時，會列出每個綁定的第一個按鍵。`app` 表無法列出。

```lisp
(def timer nil)
(add-hook 'keys-pending
  (fn (pending)
    (cancel timer)                      ; nil 是從未存在的計時器
    (when pending
      (set! timer (after 300 (fn () (message (get pending :typed) " …")))))))
```

## 正在編輯的檔案

程式可以讀取目前顯示的分頁中的檔案、編輯它並加上標記。位置是 `(line column)`，兩者都從 1 開始計算；範圍的結尾是緊接在範圍之後的位置。

| 函式 | 給出或作用 |
| --- | --- |
| `(cursor)`、`(set-cursor line column)` | 游標的位置；將它移到別處 |
| `(mode)` | `'normal` `'insert` `'visual` `'visual-line` `'visual-block` `'command` |
| `(selection)` | `(:start :end :text :kind)`，或 `nil` |
| `(line-count)`、`(buffer-line n)` | 行數；某一行的文字 |
| `(modified?)` | 是否有尚未寫入的變更 |
| `(buffer-text [start end])` | 全部文字，或兩個位置之間的文字 |
| `(visible-lines)` | 畫面上的第一行與最後一行 |
| `(insert text)` | 在游標處插入文字 |
| `(edit start end text)` | 以文字取代兩個位置之間的內容：一次復原即可撤銷 |
| `(feed-keys keys)` | 如同鍵盤一般輸入按鍵：`"ciwfoo<Esc>"` |
| `(diagnostics)` | 此檔案的診斷：`((:line :col :end-line :end-col :severity :message)...)` |
| `(all-diagnostics)` | 所有檔案的診斷，以位置表示：`((:path :file :line :col :text :severity)...)` |
| `(decorate group decorations)` | 以新的裝飾取代該群組在檔案上原有的裝飾 |

裝飾是一份名稱與值的清單。帶有 `:line` 時是針對某一行的裝飾：`:sign` 在行號旁放一個字元，`:text` 在行尾之後加上文字，`:bg` 是整行的背景色。帶有 `:start` 與 `:end` 時是針對該段文字的裝飾，可使用 `:bg` 和 `:color`。`:color` 也是標示或文字的顏色。周圍的行增減時，裝飾會留在它們的文字上。

```lisp
(decorate 'review
  (list (list :line 12 :sign "?" :color 'warning)
        (list :line 12 :text "is this still needed" :color 'muted-foreground)
        (list :start '(14 5) :end '(14 11) :bg 'warning)))
(decorate 'review nil)                  ; 全部清除
```

程式也能透過 `add-hook` 得知發生了什麼：

| Hook | 時機，以及函式被呼叫時收到的內容 |
| --- | --- |
| `buffer-entered` | 換成編輯另一個檔案：它的路徑，或 `nil` |
| `buffer-changed` | 它的文字改變了 |
| `cursor-moved` | 游標移到了別處 |
| `mode-changed` | 編輯器進入另一個模式：該模式 |
| `diagnostics-changed` | 語言伺服器有某個檔案的新消息：該檔案的路徑 |

[`examples/todo.mlisp`](../examples/todo.mlisp) 就是由這些組成的簡短程式：文字改變後讀取它，在含有 TODO 的行上加上標示，並提供兩個移動游標的命令。

程式也可以擁有一個不是檔案的文字分頁，使用它自己取的名稱：

```lisp
(open-scratch "notes")                       ; 切換過去，或建立它
(open-scratch "notes" "one\ntwo")            ; 使用這段文字，照原樣寫入
(open-scratch "notes" nil (fn (text) (message (length (lines text)) " lines")))
(scratch-name)                               ; 它顯示時為 "notes"，否則為 nil
(panes)                                      ; ((:focused true :scratch "notes" :path nil :terminal nil)...)
(scratch-tabs)                               ; 每個這樣的分頁，不論是否顯示：((:name "notes" :modified false :text "one\ntwo\n")...)
(with-pane 1 (fn () (scratch-name)))         ; 在第二個窗格中，按鍵焦點不動
```

給了函式時，`:w` 會以該分頁的文字呼叫它，如同一個命令（`:w!` 時可用 `command-bang`），而文字會維持未儲存，直到程式再次給分頁設定文字。沒給函式時，分頁中沒有會遺失的東西，也沒有可寫入的地方。在那裡 `(current-file)` 為 `nil`，而 `:w name` 會把分頁變成檔案，同時不再使用該分頁自己的按鍵。[`examples/dired.mlisp`](../examples/dired.mlisp) 在這樣的分頁中列出目錄，對檔案做 Emacs 的 dired 會做的事，並依照寫入的內容重新命名檔案。

一個分頁只存在於一個窗格中，所以一個檔案只有一個緩衝區：開啟另一個窗格已有的分頁（`open-scratch`、`open-file`、`:e`）會把它移過來，原本的窗格則顯示它之前的那個分頁。`(panes)` 從左上角開始說明每個窗格顯示什麼，`(with-pane n f)` 則把第 `n` 個窗格（從 0 開始）當作正在編輯的窗格來呼叫 `f`，而按鍵焦點維持原處：dired 用它重新讀取每個顯示中的清單。

## Terminal

程式可以在 terminal 中啟動另一個程式並與它互動；terminal 開在下方分割出的窗格中，並取得鍵盤焦點。每個 terminal 以一個數字識別：

| 函式 | 做什麼，或給出什麼 |
| --- | --- |
| `(open-terminal [command [dir]])` | 開一個新的 terminal 執行 `command`（透過 shell，即 `$SHELL -c`），或執行 shell 本身，工作目錄為 `dir` 或專案根目錄，並使用專案環境：回傳它的編號。兩者都沒給時，若有仍在執行的 terminal，就回到那一個 |
| `(terminals)` | 每個 terminal，從左上角的窗格開始：`((:id :title :command :running :exit)...)`；shell 的 `:command` 為 `nil`，`:exit` 是程式結束後的結束碼，結束前或被強制終止時為 `nil` |
| `(terminal-send id text)` | 把 `text` 送給程式，如同打字輸入（`"\r"` 是 enter）；程式已結束時為 `false` |
| `(terminal-keys id keys)` | 把按鍵送給程式，寫法同 `bind`：`"<C-c>"`、`"q<CR>"`；程式已結束時為 `false` |
| `(terminal-text id)` | 先前的輸出與目前畫面上的內容，以文字表示，每列一行 |
| `(close-terminal id)` | 關閉它的分頁並掛斷程式；沒有這個 terminal 時為 `false` |

程式結束時會以 terminal 的編號與結束碼（程式被強制終止時為 `nil`）呼叫 `terminal-exited` hook。`(panes)` 以 `:terminal` 給出窗格所顯示的 terminal 編號。

一個類似 `:make` 的命令：在專屬的 terminal 中建置專案，先關掉上一次的，鍵盤焦點回到檔案，完成時顯示訊息。

```lisp
(def make-terminal nil)
(defcommand "user.make" "Build the project in a terminal of its own"
  (fn ()
    (when make-terminal (close-terminal make-terminal))
    (set! make-terminal (open-terminal "cargo build"))
    (run "workspace.prev_pane")))
(add-hook 'terminal-exited
  (fn (id code)
    (when (= id make-terminal)
      (message (if (= code 0) "Built" (str "The build failed: " code))))))
(bind 'normal "<Space>b" "user.make")
```

## 腳本語言

`med-lisp` 是一個小型 Lisp，是初始化檔與 `:eval` 使用的語言。程式可以修改按鍵與選項、定義命令與面板（皆見上文）、以 `(load "file")` 載入另一個程式，並用 `(message ...)` 顯示訊息。整個工作階段共用一個直譯器，所以 `init.mlisp` 定義的東西可以在 `:eval` 中使用。永不結束的程式會在執行一千萬步後被停止，而不會讓視窗卡住。

不開編輯器也能試用這個語言本身：

```sh
cargo run -p med-lisp --example repl
```

```lisp
(defn leader-key (key command)       ; 詞法作用域，單一命名空間
  (bind (str "<Space>" key) command))
(map (fn (n) (* n n)) '(1 2 3))      ; => (1 4 9)
```

特殊形式：`quote if def defn fn let do set! and or cond when unless while`。`nil`（也就是空串列）和 `false` 是唯二的假值。尾端位置的呼叫不佔用堆疊。錯誤會附上發生錯誤的形式及其外層形式的行號與欄號。函式列於 `crates/med-lisp/src/builtins.rs`；目前還沒有巨集。
