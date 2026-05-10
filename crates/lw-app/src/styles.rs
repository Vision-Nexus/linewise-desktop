//! Shared CSS styles for desktop-oriented fixed pixel layout.
//! Uses CSS variables (--var) for dark/light theme support.
//! Only the main content area is flexible.

// ── Layout constants ───────────────────────────────────────────────────

pub const TOPBAR_HEIGHT: u32 = 52;
pub const SIDEBAR_WIDTH: u32 = 240;

// ── Button styles (using CSS vars for theme) ───────────────────────────

pub const BTN_PRIMARY: &str = "\
    height: 32px; padding: 0 16px; \
    background: var(--btn-primary); color: white; \
    border: none; border-radius: 6px; \
    cursor: pointer; font-size: 13px; font-weight: 500; \
    transition: background 0.15s ease, transform 0.08s ease, box-shadow 0.15s ease; \
    user-select: none;";

pub const BTN_SUCCESS: &str = "\
    height: 32px; padding: 0 16px; \
    background: var(--btn-success); color: white; \
    border: none; border-radius: 6px; \
    cursor: pointer; font-size: 13px; font-weight: 500; \
    transition: background 0.15s ease, transform 0.08s ease, box-shadow 0.15s ease; \
    user-select: none;";

pub const BTN_OUTLINE: &str = "\
    height: 32px; padding: 0 14px; \
    background: var(--btn-outline-bg); color: var(--text); \
    border: 1px solid var(--border); border-radius: 6px; \
    cursor: pointer; font-size: 13px; \
    transition: background 0.15s ease, border-color 0.15s ease, transform 0.08s ease; \
    user-select: none;";

pub const BTN_DANGER_SM: &str = "\
    height: 28px; padding: 0 10px; \
    background: transparent; color: var(--error); \
    border: 1px solid var(--error); border-radius: 4px; \
    cursor: pointer; font-size: 12px; \
    transition: background 0.15s ease, transform 0.08s ease; \
    user-select: none;";

pub const BTN_DISABLED: &str = "\
    height: 32px; padding: 0 16px; \
    background: var(--btn-disabled); color: var(--btn-disabled-text); \
    border: none; border-radius: 6px; \
    cursor: not-allowed; font-size: 13px; font-weight: 500;";

// ── Select / Input ─────────────────────────────────────────────────────

pub const SELECT: &str = "\
    height: 32px; padding: 0 8px; \
    border: 1px solid var(--input-border); border-radius: 6px; \
    font-size: 13px; background: var(--input-bg); color: var(--text); \
    transition: border-color 0.15s ease, box-shadow 0.15s ease; \
    outline: none; cursor: pointer;";

pub const INPUT: &str = "\
    height: 38px; padding: 0 12px; \
    border: 1px solid var(--input-border); border-radius: 6px; \
    font-size: 14px; background: var(--input-bg); color: var(--text); \
    transition: border-color 0.15s ease, box-shadow 0.15s ease; \
    outline: none; width: 100%; box-sizing: border-box;";
