//! Shared CSS styles for desktop-oriented fixed pixel layout.
//!
//! Desktop apps use fixed dimensions for most elements.
//! Only the main content area is flexible.

// ── Layout constants ───────────────────────────────────────────────────

pub const TOPBAR_HEIGHT: u32 = 52;
pub const SIDEBAR_WIDTH: u32 = 220;

// ── Button styles ──────────────────────────────────────────────────────

pub const BTN_PRIMARY: &str = "\
    height: 32px; padding: 0 16px; \
    background: #2563eb; color: white; \
    border: none; border-radius: 6px; \
    cursor: pointer; font-size: 13px; font-weight: 500; \
    transition: background 0.15s ease, transform 0.08s ease, box-shadow 0.15s ease; \
    user-select: none;";

pub const BTN_PRIMARY_HOVER: &str = "background: #1d4ed8; box-shadow: 0 1px 3px rgba(0,0,0,0.15);";
pub const BTN_PRIMARY_ACTIVE: &str = "background: #1e40af; transform: scale(0.97);";

pub const BTN_SUCCESS: &str = "\
    height: 32px; padding: 0 16px; \
    background: #22c55e; color: white; \
    border: none; border-radius: 6px; \
    cursor: pointer; font-size: 13px; font-weight: 500; \
    transition: background 0.15s ease, transform 0.08s ease, box-shadow 0.15s ease; \
    user-select: none;";

pub const BTN_SUCCESS_HOVER: &str = "background: #16a34a; box-shadow: 0 1px 3px rgba(0,0,0,0.15);";
pub const BTN_SUCCESS_ACTIVE: &str = "background: #15803d; transform: scale(0.97);";

pub const BTN_OUTLINE: &str = "\
    height: 32px; padding: 0 14px; \
    background: white; color: #374151; \
    border: 1px solid #d1d5db; border-radius: 6px; \
    cursor: pointer; font-size: 13px; \
    transition: background 0.15s ease, border-color 0.15s ease, transform 0.08s ease; \
    user-select: none;";

pub const BTN_OUTLINE_HOVER: &str = "background: #f9fafb; border-color: #9ca3af;";
pub const BTN_OUTLINE_ACTIVE: &str = "background: #f3f4f6; transform: scale(0.97);";

pub const BTN_DANGER_SM: &str = "\
    height: 28px; padding: 0 10px; \
    background: transparent; color: #ef4444; \
    border: 1px solid #fca5a5; border-radius: 4px; \
    cursor: pointer; font-size: 12px; \
    transition: background 0.15s ease, transform 0.08s ease; \
    user-select: none;";

pub const BTN_DANGER_SM_HOVER: &str = "background: #fef2f2; border-color: #ef4444;";
pub const BTN_DANGER_SM_ACTIVE: &str = "background: #fee2e2; transform: scale(0.97);";

pub const BTN_DISABLED: &str = "\
    height: 32px; padding: 0 16px; \
    background: #e5e7eb; color: #9ca3af; \
    border: none; border-radius: 6px; \
    cursor: not-allowed; font-size: 13px; font-weight: 500;";

// ── Select / Input ─────────────────────────────────────────────────────

pub const SELECT: &str = "\
    height: 32px; padding: 0 8px; \
    border: 1px solid #d1d5db; border-radius: 6px; \
    font-size: 13px; background: white; \
    transition: border-color 0.15s ease, box-shadow 0.15s ease; \
    outline: none; cursor: pointer;";

pub const SELECT_FOCUS: &str = "border-color: #2563eb; box-shadow: 0 0 0 2px rgba(37,99,235,0.15);";

pub const INPUT: &str = "\
    height: 38px; padding: 0 12px; \
    border: 1px solid #d1d5db; border-radius: 6px; \
    font-size: 14px; background: white; \
    transition: border-color 0.15s ease, box-shadow 0.15s ease; \
    outline: none; width: 100%; box-sizing: border-box;";

pub const INPUT_FOCUS: &str = "border-color: #2563eb; box-shadow: 0 0 0 2px rgba(37,99,235,0.15);";

// ── Card / Row ─────────────────────────────────────────────────────────

pub const CARD_ROW: &str = "\
    padding: 10px 12px; \
    border: 1px solid #e5e7eb; border-radius: 6px; \
    transition: border-color 0.15s ease, box-shadow 0.15s ease;";

pub const CARD_ROW_HOVER: &str = "border-color: #d1d5db; box-shadow: 0 1px 3px rgba(0,0,0,0.06);";
