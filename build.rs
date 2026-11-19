// Embeds the application manifest. The `msix` element ties glide.exe to the
// identity package (packaging/identity), which is what lets Windows tell Glide
// about notifications the moment they arrive. Without the package registered
// the element is ignored and Glide runs as a plain program.
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly manifestVersion="1.0" xmlns="urn:schemas-microsoft-com:asm.v1">
  <assemblyIdentity version="0.1.0.0" name="Glide.Island" type="win32"/>
  <msix xmlns="urn:schemas-microsoft-com:msix.v1" publisher="CN=Glide Dev" packageName="Glide.Island" applicationId="Glide"/>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </windowsSettings>
  </application>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/glide.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_manifest(MANIFEST);
        // Resource id 1: the exe's icon, also used for the tray.
        res.set_icon("assets/glide.ico");
        res.set("ProductName", "Glide");
        res.set("FileDescription", "Glide");
        if let Err(err) = res.compile() {
            println!("cargo:warning=could not embed Windows resources: {err}");
        }
    }
}
