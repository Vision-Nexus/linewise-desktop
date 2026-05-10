use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=input.css");
    println!("cargo:rerun-if-changed=src/");

    let status = Command::new("npx")
        .args([
            "@tailwindcss/cli",
            "-i",
            "input.css",
            "-o",
            "tailwind.generated.css",
            "--minify",
        ])
        .status();

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("cargo:warning=tailwindcss exited with {s}, using stale CSS if available");
        }
        Err(e) => {
            eprintln!("cargo:warning=tailwindcss not found ({e}), using stale CSS if available");
        }
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
