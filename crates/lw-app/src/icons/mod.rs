use dioxus::prelude::*;

#[component]
pub fn LinewiseLogo(#[props(default = "150")] width: &'static str) -> Element {
    rsx! {
        svg {
            width: "{width}",
            view_box: "0 0 280 56",
            fill: "none",
            xmlns: "http://www.w3.org/2000/svg",
            path {
                d: "M2.42566 25.7755C11.9699 32.1332 18.2561 42.9924 18.2561 55.3198H34.7938C34.7938 38.5216 26.8327 23.5848 14.4747 14.0763L2.42566 25.7755Z",
                fill: "#20026E",
            }
            path {
                d: "M18.2587 55.3224C18.2587 26.5952 41.5448 3.30908 70.2719 3.30908V19.8468C50.6779 19.8468 34.7964 35.7309 34.7964 55.3224H18.2587Z",
                fill: "url(#paint0_linear_lw)",
            }
            defs {
                linearGradient {
                    id: "paint0_linear_lw",
                    x1: "18.2587",
                    y1: "29.3144",
                    x2: "70.2719",
                    y2: "29.3144",
                    gradient_units: "userSpaceOnUse",
                    stop { stop_color: "#5C01DA" }
                    stop { offset: "1", stop_color: "#20026E" }
                }
            }
            text {
                x: "76",
                y: "46",
                fill: "#20026E",
                font_size: "36",
                font_family: "-apple-system, system-ui, sans-serif",
                font_weight: "600",
                letter_spacing: "-0.5",
                "LineWise"
            }
        }
    }
}

#[component]
pub fn LogoutIcon() -> Element {
    rsx! {
        svg {
            width: "16",
            height: "16",
            view_box: "0 0 24 24",
            fill: "none",
            xmlns: "http://www.w3.org/2000/svg",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" }
            polyline { points: "16 17 21 12 16 7" }
            line { x1: "21", y1: "12", x2: "9", y2: "12" }
        }
    }
}

#[component]
pub fn GoogleIcon() -> Element {
    rsx! {
        svg {
            width: "20",
            height: "20",
            view_box: "0 0 20 20",
            fill: "none",
            xmlns: "http://www.w3.org/2000/svg",
            path { d: "M19.6 10.2273C19.6 9.51818 19.5364 8.83636 19.4182 8.18182H10V12.05H15.3818C15.15 13.3 14.4455 14.3591 13.3864 15.0682V17.5773H16.6182C18.5091 15.8364 19.6 13.2727 19.6 10.2273Z", fill: "#4285F4" }
            path { d: "M10 20C12.7 20 14.9636 19.1045 16.6181 17.5773L13.3863 15.0682C12.4909 15.6682 11.3454 16.0227 10 16.0227C7.39545 16.0227 5.19091 14.2636 4.40455 11.9H1.06364V14.4909C2.70909 17.7591 6.09091 20 10 20Z", fill: "#34A853" }
            path { d: "M4.40455 11.9C4.20455 11.3 4.09091 10.6591 4.09091 10C4.09091 9.34091 4.20455 8.7 4.40455 8.1V5.50909H1.06364C0.386364 6.85909 0 8.38636 0 10C0 11.6136 0.386364 13.1409 1.06364 14.4909L4.40455 11.9Z", fill: "#FBBC04" }
            path { d: "M10 3.97727C11.4681 3.97727 12.7863 4.48182 13.8227 5.47273L16.6909 2.60455C14.9591 0.990909 12.6954 0 10 0C6.09091 0 2.70909 2.24091 1.06364 5.50909L4.40455 8.1C5.19091 5.73636 7.39545 3.97727 10 3.97727Z", fill: "#E94235" }
        }
    }
}

#[component]
pub fn CloseIcon() -> Element {
    rsx! {
        svg {
            width: "24",
            height: "24",
            view_box: "0 0 24 24",
            fill: "none",
            xmlns: "http://www.w3.org/2000/svg",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            line { x1: "18", y1: "6", x2: "6", y2: "18" }
            line { x1: "6", y1: "6", x2: "18", y2: "18" }
        }
    }
}

#[component]
pub fn SettingsIcon() -> Element {
    rsx! {
        svg {
            width: "16",
            height: "16",
            view_box: "0 0 24 24",
            fill: "none",
            xmlns: "http://www.w3.org/2000/svg",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            circle { cx: "12", cy: "12", r: "3" }
            path { d: "M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" }
        }
    }
}

#[component]
pub fn WrenchIcon() -> Element {
    rsx! {
        svg {
            width: "16",
            height: "16",
            view_box: "0 0 24 24",
            fill: "none",
            xmlns: "http://www.w3.org/2000/svg",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" }
        }
    }
}

#[component]
pub fn MinimizeIcon() -> Element {
    rsx! {
        svg {
            width: "16",
            height: "16",
            view_box: "0 0 24 24",
            fill: "none",
            xmlns: "http://www.w3.org/2000/svg",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            line { x1: "6", y1: "12", x2: "18", y2: "12" }
        }
    }
}

#[component]
pub fn MaximizeIcon() -> Element {
    rsx! {
        svg {
            width: "16",
            height: "16",
            view_box: "0 0 24 24",
            fill: "none",
            xmlns: "http://www.w3.org/2000/svg",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            rect { x: "6", y: "6", width: "12", height: "12", rx: "1" }
        }
    }
}

#[component]
pub fn RestoreIcon() -> Element {
    rsx! {
        svg {
            width: "16",
            height: "16",
            view_box: "0 0 24 24",
            fill: "none",
            xmlns: "http://www.w3.org/2000/svg",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            rect { x: "9", y: "5", width: "10", height: "10", rx: "1" }
            rect { x: "5", y: "9", width: "10", height: "10", rx: "1", fill: "var(--bg, white)" }
        }
    }
}

#[component]
pub fn MicrosoftIcon() -> Element {
    rsx! {
        svg {
            width: "20",
            height: "20",
            view_box: "0 0 21 21",
            fill: "none",
            xmlns: "http://www.w3.org/2000/svg",
            rect { x: "1", y: "1", width: "9", height: "9", fill: "#F25022" }
            rect { x: "11", y: "1", width: "9", height: "9", fill: "#7FBA00" }
            rect { x: "1", y: "11", width: "9", height: "9", fill: "#00A4EF" }
            rect { x: "11", y: "11", width: "9", height: "9", fill: "#FFB900" }
        }
    }
}

// ── Action icons (lucide, 14px, currentColor) — for button/label parity with the
//    wave design system. Sized 14px to sit in the dense row/toolbar buttons. ──

#[component]
pub fn UploadIcon() -> Element {
    rsx! {
        svg {
            width: "14", height: "14", view_box: "0 0 24 24", fill: "none",
            xmlns: "http://www.w3.org/2000/svg", stroke: "currentColor",
            stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
            path { d: "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" }
            polyline { points: "17 8 12 3 7 8" }
            line { x1: "12", y1: "3", x2: "12", y2: "15" }
        }
    }
}

#[component]
pub fn AlertTriangleIcon() -> Element {
    rsx! {
        svg {
            width: "14", height: "14", view_box: "0 0 24 24", fill: "none",
            xmlns: "http://www.w3.org/2000/svg", stroke: "currentColor",
            stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
            path { d: "m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3Z" }
            path { d: "M12 9v4" }
            path { d: "M12 17h.01" }
        }
    }
}

#[component]
pub fn PauseIcon() -> Element {
    rsx! {
        svg {
            width: "14", height: "14", view_box: "0 0 24 24", fill: "none",
            xmlns: "http://www.w3.org/2000/svg", stroke: "currentColor",
            stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
            rect { x: "14", y: "4", width: "4", height: "16", rx: "1" }
            rect { x: "6", y: "4", width: "4", height: "16", rx: "1" }
        }
    }
}

#[component]
pub fn PlayIcon() -> Element {
    rsx! {
        svg {
            width: "14", height: "14", view_box: "0 0 24 24", fill: "none",
            xmlns: "http://www.w3.org/2000/svg", stroke: "currentColor",
            stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
            polygon { points: "6 3 20 12 6 21 6 3" }
        }
    }
}

#[component]
pub fn RetryIcon() -> Element {
    rsx! {
        svg {
            width: "14", height: "14", view_box: "0 0 24 24", fill: "none",
            xmlns: "http://www.w3.org/2000/svg", stroke: "currentColor",
            stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
            path { d: "M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" }
            path { d: "M3 3v5h5" }
        }
    }
}

#[component]
pub fn TrashIcon() -> Element {
    rsx! {
        svg {
            width: "14", height: "14", view_box: "0 0 24 24", fill: "none",
            xmlns: "http://www.w3.org/2000/svg", stroke: "currentColor",
            stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
            path { d: "M3 6h18" }
            path { d: "M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" }
            line { x1: "10", y1: "11", x2: "10", y2: "17" }
            line { x1: "14", y1: "11", x2: "14", y2: "17" }
        }
    }
}

#[component]
pub fn ChevronRightIcon() -> Element {
    rsx! {
        svg {
            width: "14", height: "14", view_box: "0 0 24 24", fill: "none",
            xmlns: "http://www.w3.org/2000/svg", stroke: "currentColor",
            stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
            path { d: "m9 18 6-6-6-6" }
        }
    }
}

#[component]
pub fn ChevronDownIcon() -> Element {
    rsx! {
        svg {
            width: "14", height: "14", view_box: "0 0 24 24", fill: "none",
            xmlns: "http://www.w3.org/2000/svg", stroke: "currentColor",
            stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
            path { d: "m6 9 6 6 6-6" }
        }
    }
}

// Loader2 (lucide) — spins via the global `.lw-spin` keyframe. Used as the
// batch-overview "active work" indicator. 12px to sit in the compact header.
#[component]
pub fn SpinnerIcon() -> Element {
    rsx! {
        svg {
            class: "lw-spin",
            width: "12", height: "12", view_box: "0 0 24 24", fill: "none",
            xmlns: "http://www.w3.org/2000/svg", stroke: "currentColor",
            stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
            path { d: "M21 12a9 9 0 1 1-6.219-8.56" }
        }
    }
}

// CheckCircle2 (lucide) — batch-complete indicator.
#[component]
pub fn CheckCircleIcon() -> Element {
    rsx! {
        svg {
            width: "12", height: "12", view_box: "0 0 24 24", fill: "none",
            xmlns: "http://www.w3.org/2000/svg", stroke: "currentColor",
            stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
            path { d: "M22 11.08V12a10 10 0 1 1-5.93-9.14" }
            path { d: "m9 11 3 3L22 4" }
        }
    }
}

// Wifi (lucide) — network status pill, connected/degraded.
#[component]
pub fn WifiIcon() -> Element {
    rsx! {
        svg {
            width: "12", height: "12", view_box: "0 0 24 24", fill: "none",
            xmlns: "http://www.w3.org/2000/svg", stroke: "currentColor",
            stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
            path { d: "M12 20h.01" }
            path { d: "M2 8.82a15 15 0 0 1 20 0" }
            path { d: "M5 12.859a10 10 0 0 1 14 0" }
            path { d: "M8.5 16.429a5 5 0 0 1 7 0" }
        }
    }
}

// WifiOff (lucide) — network status pill, offline.
#[component]
pub fn WifiOffIcon() -> Element {
    rsx! {
        svg {
            width: "12", height: "12", view_box: "0 0 24 24", fill: "none",
            xmlns: "http://www.w3.org/2000/svg", stroke: "currentColor",
            stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
            path { d: "M12 20h.01" }
            path { d: "M8.5 16.429a5 5 0 0 1 7 0" }
            path { d: "M5 12.859a10 10 0 0 1 5.17-2.69" }
            path { d: "M19 12.859a10 10 0 0 0-2.007-1.523" }
            path { d: "M2 8.82a15 15 0 0 1 4.177-2.643" }
            path { d: "M22 8.82a15 15 0 0 0-11.288-3.764" }
            path { d: "m2 2 20 20" }
        }
    }
}
