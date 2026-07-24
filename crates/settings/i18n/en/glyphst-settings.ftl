app-name = Glyphst

## Common

cancel = Cancel
cancel-selection = Cancel Selection
copy = Copy
markdown-copy-code = Copy Code
markdown-wrap-code = Wrap Code Block
markdown-unwrap-code = Unwrap Code Block
current = Current
delete = Delete
edit = Edit
escape-key = Esc
export = Export
open-pdf = Open Document
add = Add
remove = Remove
reset-zoom = Reset Zoom
save = Save
select = Select
select-all = Select All
sending = Sending…
send = Send
untitled = Untitled
view = View
zoom-in = Zoom In
zoom-out = Zoom Out
actions-menu = Actions ▾
selected-count =
    { $count ->
        [one] { $count } selected
       *[other] { $count } selected
    }
item-count =
    { $count ->
        [one] { $count } item
       *[other] { $count } items
    }
page-title = Page { $page }

## Pickers

bookmarks-placeholder = Open bookmark…
bookmarks-empty = No bookmarks found
command-palette-placeholder = Execute a command…
command-palette-empty = No commands found
go-to-page-placeholder = Go to page…
go-to-page-empty = Enter a page from 1 to { $page_count }
history-placeholder = Open recent PDF…
history-empty = No recent PDFs found
notes-placeholder = Go to note…
notes-empty = No notes found
notes-title = Notes
search-document-placeholder = Search document…
searchable-dropdown-empty = No matching items
theme-selector-placeholder = Select Theme…
theme-selector-empty = No themes found
toc-placeholder = Go to table of contents…
toc-empty = No table of contents entries found
table-of-contents-title = Table of Contents
no-matching-models = No matching models

## PDF view

pdf-context-add-note = Add Note
pdf-context-change-highlight-color = Change Highlight Color
pdf-context-copy-image = Copy Image
pdf-context-copy-region = Copy Region
pdf-context-copy-plain-text = Copy in Plain Text
pdf-context-copy-markdown = Copy in Markdown
pdf-context-edit-note = Edit Note
pdf-context-enter-full-screen = Enter Full Screen
pdf-context-exit-full-screen = Exit Full Screen
pdf-context-highlight = Highlight
pdf-context-remove-highlight = Remove Highlight
pdf-context-remove-note = Remove Note
pdf-context-save-image = Save Image
pdf-context-save-page-as-image = Save Page as Image
pdf-context-save-region = Save Region
pdf-context-translate-selection = Translate
pdf-export-as-image = Export as Image
pdf-export-as-text = Export as Text
pdf-failed-to-load = Failed to load document
pdf-loading = Loading { $name }
pdf-no-pages-available = No pages available
pdf-page-overview-title = Page Overview
pdf-region-dismiss = Dismiss Region
pdf-region-select-tooltip = Select Image Region
pdf-thumbnails-title = Thumbnails
epub-appearance-title = Book Appearance
epub-font-size = Font size
epub-line-height = Line height
epub-horizontal-margin = Horizontal margin
epub-max-content-width = Maximum content width
epub-theme = Theme
epub-theme-light = Light
epub-theme-sepia = Sepia
epub-theme-dark = Dark
epub-publisher-styles = Publisher styles
epub-enabled = Enabled
epub-disabled = Disabled
epub-reset-global-defaults = Reset to global defaults
translation-loading = Translating…
translation-no-result = Translation provider returned no text
translation-title = Translation
translation-unpin = Unpin
translation-close = Close

## AI

ai-title = AI
ai-ask-placeholder = Ask about this PDF…
ai-new-chat = New chat
ai-no-current-page-text = No current page text is available.
ai-no-document-text = No extracted text is available for this PDF.
ai-no-document-image = No document images are available for this PDF.
ai-no-region-image = No PDF image region is currently selected.
ai-no-previous-sessions = No previous AI sessions for this PDF
ai-no-selected-text = No selected text is currently available.
ai-mention-preview-truncated = Preview limited to 4,000 characters. The complete attached context is still sent to AI.
ai-search-models-placeholder = Type to search models…
ai-select-provider-model = Select provider/model
ai-copied = Copied
ai-copy-message = Copy Message
ai-copy-response = Copy This Agent Response
ai-copy-selection = Copy Selection
ai-copy-link = Copy Link
ai-copy-error = Copy Error Message
ai-regenerate-from-message = Regenerate from Here
ai-regenerate-from-message-warning = Regenerate from Here (removes later messages)
ai-retry-response = Retry Response
ai-retry-response-warning = Retry Response (removes later messages)
ai-submit-edit = Submit Edit and Regenerate
ai-dismiss-error = Dismiss Error
ai-scroll-top = Scroll to Top
ai-scroll-bottom = Scroll to Bottom
ai-scroll-latest-user = Scroll to Most Recent User Message
ai-stop-generation = Stop Generation
ai-show-chat = Show Chat
ai-show-history = Show Chat History
ai-close-panel = Close AI Panel
ai-edit-message-placeholder = Edit this message…
ai-edit-context-unavailable = This saved message cannot be edited safely because its attached context is ambiguous.
attach = Attach
mention-current-page-text = Current page text
mention-current-page-text-description = Attach text from the visible page
mention-page-image-description = Attach a page as an image
mention-selection = Selection
mention-selection-description = Attach the selected PDF text
mention-document-text = Whole PDF text
mention-document-text-description = Attach extracted text from the PDF
mention-document-image = Whole PDF as images
mention-document-image-description = Attach every PDF page as an image
mention-region-image = Selected image region
mention-region-image-description = Attach the selected PDF region as an image

## Settings

settings-title = Settings
settings-window-title = Glyphst Settings
settings-tab-general = General
settings-tab-keymap = Keymap
settings-tab-appearance = Appearance
settings-tab-model-providers = Model Providers
settings-tab-translation = Translation
settings-tab-ocr = OCR
settings-section-general = General
settings-section-language = Language
settings-section-zoom = Zoom
settings-section-scroll = Scroll
settings-section-copy = Copy
settings-section-reading = EPUB Reading
settings-copy-format = Default format
settings-copy-plain-text = Plain Text
settings-copy-markdown = Markdown
settings-copy-semantic = Semantic copy
settings-reading-font-size = Default font size
settings-reading-line-height = Default line height
settings-reading-margin = Horizontal margin
settings-reading-width = Maximum content width
settings-reading-publisher-styles = Use publisher styles
settings-section-keymap = Keymap
settings-section-window = Window
settings-section-tabs = Tabs
settings-section-theme = Theme
settings-section-fonts = Fonts
settings-section-overrides = Overrides
settings-section-model-providers = Model Providers
settings-search-fonts-placeholder = Search fonts…
settings-model-id-placeholder = model id…
settings-api-key-placeholder = paste API key…
settings-search-model-placeholder = Search model…
settings-search-languages-placeholder = Search languages…
settings-search-themes-placeholder = Search themes…
settings-search-light-themes-placeholder = Search light themes…
settings-search-dark-themes-placeholder = Search dark themes…
settings-connection-succeeded = Connection succeeded.
settings-connection-failed = Connection failed: { $error }
settings-fetching-models = Fetching models…
settings-fetched-models =
    { $count ->
        [one] Fetched { $count } model.
       *[other] Fetched { $count } models.
    }
settings-fetch-models-failed = Failed to fetch models: { $error }
settings-testing-connection = Testing connection…
settings-exported-to = Exported to { $path }
settings-reset-fonts = Reset fonts
settings-ui-font-family = UI font family
settings-no-matching-fonts = No matching fonts
settings-default = Default
settings-api-key = API Key
settings-api-host = API Host
settings-language = Language
settings-base-preset = Base preset
settings-keymap-editor = Keymap editor
settings-open-keymap = Open Keymap
settings-background = Background
settings-tab-bar = Tab bar
settings-default-models = Default models
settings-main-model = Main model
settings-title-model = Title model
settings-translation-model = Translation model
settings-open-translation-model = Configure the AI translation model in AI → General.
settings-model-list-hint = Only models from enabled providers' model lists appear here.
settings-page-context = Page context
settings-page-text-format = Text format
settings-page-text-format-block = Block
settings-page-text-format-semantic = Semantic
settings-page-text-format-hint = Block preserves extracted text blocks. Semantic organizes text by document structure.
settings-page-image-quality = Image quality
settings-page-image-quality-low = Low
settings-page-image-quality-medium = Medium
settings-page-image-quality-high = High
settings-page-image-quality-hint = Low: 1024 px / JPEG 65 · Medium: 1600 px / JPEG 75 · High: 2400 px / JPEG 85
settings-page-image-limit = Image page limit
settings-page-image-limit-hint = Maximum number of page images that can be attached to one AI request.
settings-quick-ai-chat = Quick AI chat
settings-no-enabled-provider-models = No enabled provider models
settings-no-models-added = No models added yet.
settings-no-models-returned = No models returned.
settings-no-matching-models-period = No matching models.
settings-add-models = Add Models…
settings-add-model-id = Add ID
settings-add-provider-models = Add { $provider } Models
settings-model-catalog-hint = Select one or more models. Provider catalogs are cached while this settings window remains open.
settings-refresh-models = Refresh Models
settings-add-selected-models = Add Selected ({ $count })
settings-select-all = Select All
settings-clear-selection = Clear
settings-model-already-added = Added
settings-no-cached-models = No catalog cached
settings-model-catalog-empty = The model catalog has not been loaded yet. Refresh to fetch it from the provider.
settings-models-cached =
    { $count ->
        [one] { $count } model cached
       *[other] { $count } models cached
    }
settings-added-models =
    { $count ->
        [one] Added { $count } model.
       *[other] Added { $count } models.
    }
settings-model-list = Model List
settings-theme-mode = Theme Mode
settings-mode = Mode
settings-light-theme = Light Theme
settings-dark-theme = Dark Theme
settings-font-fallbacks-hint = Separate fallback font names with commas.
settings-ocr-behavior = OCR Behavior
settings-ocr-mode-always = Always
settings-ocr-mode-auto = Auto
settings-ocr-mode-off = Off
settings-ocr-mode-hint = Always processes pages with OCR. Auto uses OCR when a page has no usable embedded text. Off disables OCR.
settings-ocr-provider = OCR Provider
settings-ocr-unlimited = Baidu Unlimited-OCR
settings-ocr-paddle = PaddleOCR
settings-ocr-paddle-token = Paddle token
settings-ocr-paddle-token-placeholder = paste Paddle token…
settings-ocr-unlimited-api-key = API key
settings-ocr-unlimited-api-key-placeholder = paste Baidu API key…
settings-ocr-unlimited-secret-key = Secret key
settings-ocr-unlimited-secret-key-placeholder = paste Baidu secret key…
settings-ocr-endpoint = API endpoint
settings-ocr-model = Model
settings-test-connection = Test Connection
settings-ocr-testing = Testing…
settings-ocr-connection-testing = Testing the selected OCR provider…
settings-ocr-connection-succeeded = Connection succeeded.
settings-ocr-connection-invalid-auth = Authentication failed. Check the selected provider credentials.
settings-ocr-connection-retryable = The connection could not be completed. Check the network and try again.
settings-ocr-connection-provider-error = The OCR provider returned an unexpected error.
settings-ocr-connection-provider-error-code = The OCR provider returned error code { $code }.
settings-ocr-paddle-connection-hint = The test performs a read-only lookup for a random nonexistent Paddle job.
settings-ocr-unlimited-connection-hint = The test validates the Baidu credentials without submitting a document.
language-system = System
language-english = English
language-simplified-chinese = Simplified Chinese

## New tab

new-tab-title = New Tab
new-tab-empty-title = No recent PDFs
new-tab-empty-description = Open a PDF to add it to history.
new-tab-last-read = Last read: p. { $page }
new-tab-last-read-progress = Last read: { $progress }%
new-tab-preview-unavailable = Preview unavailable
new-tab-recent-pdfs = Recent Documents
new-tab-rendering-preview = Rendering preview…

## Notes

note-title = Note
note-editor-help = Ctrl+Enter saves · Esc cancels
no-selected-text = No selected text
untitled-note = Untitled note

## Toolbar

toolbar-menu-glyphst = Glyphst
toolbar-menu-file = File
toolbar-menu-tab = Tab
toolbar-about-glyphst = About Glyphst
toolbar-check-updates = Check for Updates
toolbar-open-settings = Open Settings
toolbar-quit-glyphst = Quit Glyphst
toolbar-open-file = Open File
toolbar-new-tab = New Tab
toolbar-close-tab = Close Tab
toolbar-split-right = Split Right
toolbar-split-down = Split Down

## Keymap

keymap-title = Keymap
keymap-add-keybinding = Add Keybinding
keymap-copy-action = Copy Action
keymap-copy-context = Copy Context
keymap-edit-keystroke = Edit Keystroke
keymap-empty = No keybindings found
keymap-error-press-shortcut = Press a shortcut before saving.
keymap-header-action = Action
keymap-header-context = Context
keymap-header-keystrokes = Keystrokes
keymap-header-source = Source
keymap-reset-to-default = Reset to Default
keymap-search-placeholder = Search keymap…
keymap-source-default = Default
keymap-source-user = User
keymap-unbound = Unbound
keymap-modal-subtitle = { $action } · { $context }
keymap-conflicting-bindings =
    { $count ->
        [one] There is { $count } binding with the same keystrokes.
       *[other] There are { $count } bindings with the same keystrokes.
    }
keymap-context = Context: { $context }
keymap-preset-modern = Modern
keymap-preset-vim = Vim

## Thinking levels

thinking-level-off = Off
thinking-level-minimal = Minimal
thinking-level-low = Low
thinking-level-medium = Medium
thinking-level-high = High
thinking-level-x-high = X High
thinking-level-max = Max
thinking-level-off-description = No extra reasoning
thinking-level-minimal-description = Minimal reasoning
thinking-level-low-description = Fast light reasoning
thinking-level-medium-description = Balanced reasoning
thinking-level-high-description = Deeper reasoning
thinking-level-x-high-description = Maximum OpenAI reasoning
thinking-level-max-description = Maximum Anthropic thinking

## Keybinding categories

keybinding-category-workspace = Workspace
keybinding-category-pdf-view = Document Reader
keybinding-category-page-overview = Page Overview
keybinding-category-new-tab = New Tab
keybinding-category-search-results = Search Results
keybinding-category-workspace-description = App-wide commands such as files, tabs, panes, settings, and panels.
keybinding-category-pdf-view-description = Reading commands for the active document.
keybinding-category-page-overview-description = Commands used while the page overview grid is open.
keybinding-category-new-tab-description = Commands used while the new tab view is focused.
keybinding-category-search-results-description = Commands used while the search box is focused.

## Keybinding commands

keybinding-command-workspace-toggle-command-palette = Toggle Command Palette
keybinding-command-workspace-open-file = Open File
keybinding-command-workspace-open-new-tab = Open New Tab
keybinding-command-workspace-close-tab = Close Tab
keybinding-command-workspace-open-settings = Open Settings
keybinding-command-workspace-open-keybindings = Open Keybindings
keybinding-command-workspace-toggle-full-screen = Toggle Full Screen
keybinding-command-workspace-toggle-transparency = Toggle Transparency
keybinding-command-workspace-toggle-theme = Toggle Theme
keybinding-command-workspace-toggle-theme-selector = Toggle Theme Selector
keybinding-command-workspace-toggle-ai-panel = Toggle AI Panel
keybinding-command-workspace-open-history = Open History
keybinding-command-workspace-open-bookmarks = Open Bookmarks
keybinding-command-workspace-toggle-thumbnails = Toggle Page Thumbnails
keybinding-command-workspace-toggle-toc = Toggle Table of Contents
keybinding-command-workspace-open-notes = Open Notes
keybinding-command-workspace-toggle-notes = Toggle Notes Sidebar
keybinding-command-workspace-split-right = Split Right
keybinding-command-workspace-split-left = Split Left
keybinding-command-workspace-split-up = Split Up
keybinding-command-workspace-split-down = Split Down
keybinding-command-workspace-split-vertical = Split Vertically
keybinding-command-workspace-split-horizontal = Split Horizontally
keybinding-command-workspace-activate-pane-left = Focus Pane Left
keybinding-command-workspace-activate-pane-right = Focus Pane Right
keybinding-command-workspace-activate-pane-up = Focus Pane Up
keybinding-command-workspace-activate-pane-down = Focus Pane Down
keybinding-command-workspace-activate-next-pane = Focus Next Pane
keybinding-command-workspace-activate-previous-pane = Focus Previous Pane
keybinding-command-workspace-swap-pane-left = Swap Pane Left
keybinding-command-workspace-swap-pane-right = Swap Pane Right
keybinding-command-workspace-swap-pane-up = Swap Pane Up
keybinding-command-workspace-swap-pane-down = Swap Pane Down
keybinding-command-workspace-move-pane-left = Move Pane Left
keybinding-command-workspace-move-pane-right = Move Pane Right
keybinding-command-workspace-move-pane-up = Move Pane Up
keybinding-command-workspace-move-pane-down = Move Pane Down
keybinding-command-workspace-resize-pane-left = Resize Pane Left
keybinding-command-workspace-resize-pane-right = Resize Pane Right
keybinding-command-workspace-resize-pane-up = Resize Pane Up
keybinding-command-workspace-resize-pane-down = Resize Pane Down
keybinding-command-workspace-reset-pane-sizes = Reset Pane Sizes
keybinding-command-workspace-close-other-panes = Close Other Panes
keybinding-command-pdf-view-copy-selection = Copy Selection
keybinding-command-pdf-view-undo = Undo
keybinding-command-pdf-view-redo = Redo
keybinding-command-pdf-view-select-all = Select All
keybinding-command-pdf-view-zoom-in = Zoom In
keybinding-command-pdf-view-zoom-out = Zoom Out
keybinding-command-pdf-view-reset-zoom = Reset Zoom
keybinding-command-pdf-view-reload = Reload Document
keybinding-command-pdf-view-toggle-presentation-mode = Toggle Presentation Mode
keybinding-command-pdf-view-search = Search
keybinding-command-pdf-view-next-search-result = Next Search Result
keybinding-command-pdf-view-prev-search-result = Previous Search Result
keybinding-command-search-results-next-search-result = Next Search Result
keybinding-command-search-results-prev-search-result = Previous Search Result
keybinding-command-pdf-view-next-page = Next Page
keybinding-command-pdf-view-previous-page = Previous Page
keybinding-command-pdf-view-next-half-page = Next Half Page
keybinding-command-pdf-view-previous-half-page = Previous Half Page
keybinding-command-pdf-view-scroll-line-down = Scroll Line Down
keybinding-command-pdf-view-scroll-line-up = Scroll Line Up
keybinding-command-pdf-view-go-to-beginning = Go to Beginning
keybinding-command-pdf-view-go-to-end = Go to End
keybinding-command-pdf-view-jump-backward = Jump Backward
keybinding-command-pdf-view-jump-forward = Jump Forward
keybinding-command-pdf-view-toc = Open Table of Contents
keybinding-command-pdf-view-open-quick-ask = Open Quick Ask
keybinding-command-pdf-view-go-to-page = Go to Page
keybinding-command-pdf-view-add-bookmark = Add Bookmark
keybinding-command-pdf-view-toggle-grid-overview = Toggle Page Overview
keybinding-command-page-overview-zoom-in = Zoom In
keybinding-command-page-overview-zoom-out = Zoom Out
keybinding-command-page-overview-next-page = Next Page
keybinding-command-page-overview-previous-page = Previous Page
keybinding-command-page-overview-next-half-page = Next Half Page
keybinding-command-page-overview-previous-half-page = Previous Half Page
keybinding-command-page-overview-go-to-beginning = Go to Beginning
keybinding-command-page-overview-go-to-end = Go to End
keybinding-command-page-overview-grid-cursor-up = Move Up
keybinding-command-page-overview-grid-cursor-down = Move Down
keybinding-command-page-overview-grid-cursor-left = Move Left
keybinding-command-page-overview-grid-cursor-right = Move Right
keybinding-command-page-overview-grid-overview-confirm = Open Selected Page
keybinding-command-page-overview-toggle-grid-overview = Close Page Overview
keybinding-command-new-tab-scroll-line-down = Scroll Line Down
keybinding-command-new-tab-scroll-line-up = Scroll Line Up
keybinding-command-new-tab-next-half-page = Next Half Page
keybinding-command-new-tab-previous-half-page = Previous Half Page
keybinding-command-new-tab-enter-select = Enter Selection Mode
keybinding-command-new-tab-cancel-select = Cancel Selection
keybinding-command-new-tab-delete-selected = Delete Selected
