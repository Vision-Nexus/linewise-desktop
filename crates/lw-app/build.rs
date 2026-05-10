use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=input.css");
    println!("cargo:rerun-if-changed=src/");

    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/C", "npx"]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = Command::new("npx");

    let status = cmd
        .args([
            "@tailwindcss/cli",
            "-i",
            "input.css",
            "-o",
            "tailwind.generated.css",
            "--minify",
        ])
        .status();

    let tailwind_out = std::path::Path::new("tailwind.generated.css");
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("tailwindcss exited with {s}"),
        Err(e) if tailwind_out.exists() => {
            eprintln!("cargo:warning=tailwindcss not found ({e}), using stale CSS");
        }
        Err(e) => panic!("tailwindcss not found ({e}) and no cached tailwind.generated.css"),
    }

    #[cfg(target_os = "linux")]
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib/linewise-desktop");

    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");

    #[cfg(target_os = "windows")]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icons/icon.ico");
        res.compile().expect("failed to compile Windows resources");
    }
}
