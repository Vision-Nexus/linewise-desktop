/// CSS for the chat panel and markdown content.
///
/// Uses CSS variable tokens defined by the host app's global stylesheet.
/// Variables used: --bg, --bg-secondary, --bg-tertiary, --text, --text-secondary,
/// --border, --border-hover, --btn-primary, --input-bg, --input-border,
/// --border-focus, --shadow-sm.
pub const CHAT_CSS: &str = r#"
/* Chat panel layout */
.chat-panel {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg);
    color: var(--text);
}

.chat-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 12px;
    font-weight: 600;
    font-size: 15px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
}

.chat-header-actions {
    display: flex;
    align-items: center;
    gap: 4px;
}

.chat-header-btn {
    width: 28px;
    height: 28px;
    border: 1px solid var(--border);
    border-radius: 5px;
    background: var(--bg);
    color: var(--text);
    cursor: pointer;
    font-size: 15px;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: background 0.15s;
}
.chat-header-btn:hover { background: var(--bg-tertiary); }
.chat-header-btn.active { background: var(--bg-tertiary); border-color: var(--border-hover); }

/* History dropdown */
.chat-history-wrapper { position: relative; }

.chat-history-dropdown {
    position: absolute;
    top: 34px;
    right: 0;
    width: 260px;
    max-height: 360px;
    overflow-y: auto;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: var(--shadow-md);
    z-index: 100;
    padding: 4px 0;
}

.chat-history-title {
    padding: 8px 12px 4px;
    font-size: 11px;
    font-weight: 600;
    color: var(--text-secondary);
    text-transform: uppercase;
    letter-spacing: 0.05em;
}

.chat-history-empty {
    padding: 16px 12px;
    font-size: 12px;
    color: var(--text-secondary);
    text-align: center;
}

.chat-history-item {
    display: flex;
    align-items: center;
    padding: 7px 12px;
    cursor: pointer;
    font-size: 13px;
    transition: background 0.1s;
    gap: 4px;
}
.chat-history-item:hover { background: var(--bg-tertiary); }
.chat-history-item.active { background: var(--bg-tertiary); font-weight: 600; }

.chat-history-item-title {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.chat-history-delete {
    width: 20px;
    height: 20px;
    border: none;
    border-radius: 3px;
    background: transparent;
    color: var(--text-secondary);
    cursor: pointer;
    font-size: 14px;
    display: flex;
    align-items: center;
    justify-content: center;
    opacity: 0;
    transition: opacity 0.1s, background 0.1s;
}
.chat-history-item:hover .chat-history-delete { opacity: 1; }
.chat-history-delete:hover { background: var(--border); color: var(--text); }

.chat-messages {
    flex: 1;
    overflow-y: auto;
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
}

.chat-empty {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: var(--text-secondary);
    font-size: 14px;
}

/* Chat bubbles */
.bubble {
    max-width: 85%;
    padding: 10px 14px;
    border-radius: 12px;
    line-height: 1.5;
    font-size: 14px;
    word-wrap: break-word;
}

.bubble-user {
    background: var(--btn-primary);
    color: white;
    margin-left: auto;
    border-bottom-right-radius: 4px;
}

.bubble-assistant {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    color: var(--text);
    margin-right: auto;
    border-bottom-left-radius: 4px;
}

/* Tool call cards */
.tool-card {
    background: var(--bg-tertiary);
    border-radius: 8px;
    padding: 8px 12px;
    margin: 4px 0;
    font-size: 13px;
    border-left: 3px solid var(--btn-primary);
}

.tool-card-header {
    font-weight: 600;
    margin-bottom: 4px;
}

.tool-card-result {
    font-size: 12px;
    color: var(--text-secondary);
    max-height: 100px;
    overflow-y: auto;
}

/* Streaming indicator */
.streaming-cursor {
    display: inline-block;
    width: 8px;
    height: 16px;
    background: var(--btn-primary);
    animation: blink 1s step-end infinite;
    vertical-align: text-bottom;
    margin-left: 2px;
}

@keyframes blink {
    50% { opacity: 0; }
}

/* Chat input */
.chat-input {
    display: flex;
    gap: 8px;
    padding: 12px 16px;
    border-top: 1px solid var(--border);
    flex-shrink: 0;
}

.chat-input textarea {
    flex: 1;
    resize: none;
    border: 1px solid var(--input-border);
    border-radius: 8px;
    padding: 8px 12px;
    font-size: 14px;
    font-family: inherit;
    background: var(--input-bg);
    color: var(--text);
    min-height: 40px;
    max-height: 120px;
    outline: none;
}

.chat-input textarea:focus {
    border-color: var(--border-focus);
}

.chat-input button {
    padding: 8px 16px;
    border: none;
    border-radius: 8px;
    background: var(--btn-primary);
    color: white;
    font-weight: 600;
    cursor: pointer;
    font-size: 14px;
    align-self: flex-end;
    transition: opacity 0.15s;
}

.chat-input button:hover {
    opacity: 0.9;
}

.chat-input button:disabled {
    background: var(--btn-disabled);
    color: var(--btn-disabled-text);
    cursor: not-allowed;
    opacity: 1;
}

/* Markdown content — inherits color from parent bubble */
.md-content { color: inherit; }

.md-content h1 { font-size: 1.5em; font-weight: 700; margin: 12px 0 6px; }
.md-content h2 { font-size: 1.3em; font-weight: 700; margin: 10px 0 5px; }
.md-content h3 { font-size: 1.15em; font-weight: 600; margin: 8px 0 4px; }
.md-content h4, .md-content h5, .md-content h6 { font-size: 1em; font-weight: 600; margin: 6px 0 3px; }

.md-content p { margin: 4px 0; }

.md-content code {
    background: var(--bg-tertiary);
    padding: 1px 5px;
    border-radius: 3px;
    font-family: 'SF Mono', 'Fira Code', 'Cascadia Code', monospace;
    font-size: 0.9em;
}

.md-content pre {
    background: var(--bg-tertiary);
    padding: 10px 12px;
    border-radius: 6px;
    overflow-x: auto;
    margin: 6px 0;
}

.md-content pre code {
    background: none;
    padding: 0;
    font-size: 13px;
    line-height: 1.5;
}

.md-content blockquote {
    border-left: 3px solid var(--border);
    padding-left: 12px;
    margin: 6px 0;
    color: var(--text-secondary);
}

.md-content ul, .md-content ol {
    padding-left: 20px;
    margin: 4px 0;
}

.md-content li { margin: 2px 0; }

.md-content a {
    color: var(--btn-primary);
    text-decoration: underline;
}

.md-content img {
    max-width: 100%;
    border-radius: 6px;
    margin: 4px 0;
}

.md-content table {
    border-collapse: collapse;
    width: 100%;
    margin: 6px 0;
    font-size: 13px;
}

.md-content th, .md-content td {
    border: 1px solid var(--border);
    padding: 4px 8px;
    text-align: left;
}

.md-content th {
    font-weight: 600;
    background: var(--bg-tertiary);
}

.md-content hr {
    border: none;
    border-top: 1px solid var(--border);
    margin: 10px 0;
}

.md-content input[type="checkbox"] {
    margin-right: 4px;
}

.md-content s { text-decoration: line-through; }

/* Scroll-to-bottom button */
.chat-scroll-btn {
    position: absolute;
    bottom: 8px;
    right: 16px;
    width: 32px;
    height: 32px;
    border-radius: 50%;
    border: 1px solid var(--border);
    background: var(--bg);
    color: var(--text-secondary);
    font-size: 16px;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    box-shadow: var(--shadow-sm);
    transition: background 0.15s, color 0.15s, box-shadow 0.15s;
    z-index: 10;
}
.chat-scroll-btn:hover {
    background: var(--bg-tertiary);
    color: var(--text);
    box-shadow: var(--shadow-md);
}
"#;
