fn main() {
    // Embed the application icon (assets/voice.ico) into voice-mail.exe
    // so Explorer, taskbar, and the title bar show it. Paths are relative
    // to this file's package root (app/).
    let _ = embed_resource::compile("../assets/app.rc", embed_resource::NONE);

    // Main-thread stack reserve: the Settings page tree is deep, and its
    // render+layout recursion exhausts the 1MB OS default in debug builds
    // (stack overflow at first paint; release survived on leaner frames —
    // bisected 2026-09-17, size-dependent: Output/static fits, Settings
    // does not). 8MB *reserve* costs no memory until touched; it is not
    // papering over runaway recursion (release completes the same bounded
    // work). Scoped here so only this binary (not the backend) is affected.
    println!("cargo:rustc-link-arg=/STACK:8388608");
}