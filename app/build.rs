fn main() {
    // Embed the application icon (assets/voice.ico) into present-voice.exe
    // so Explorer, taskbar, and the title bar show it. Paths are relative
    // to this file's package root (app/).
    let _ = embed_resource::compile("../assets/app.rc", embed_resource::NONE);
}