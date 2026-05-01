fn main() {
    // Embed Windows application manifest with PerMonitorV2 DPI awareness.
    // When FFmpeg is launched as a child of this process it inherits the DPI
    // context, giving ddagrab full-rate DXGI Desktop Duplication capture
    // instead of the ~17fps throttle caused by a DPI-unaware parent.
    let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity version="1.0.0.0" processorArchitecture="amd64"
    name="ClipLite" type="win32"/>

  <!-- Common Controls v6 — required for TaskDialogIndirect and modern UI.
       Without this the OS loads comctl32 v5 which lacks TaskDialog. -->
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"/>
    </dependentAssembly>
  </dependency>

  <!-- Windows 10/11 compatibility — required for ddagrab to deliver frames
       at the full monitor refresh rate as a DXGI Desktop Duplication client. -->
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>

  <!-- PerMonitorV2 DPI awareness — inherited by FFmpeg child processes.
       Without this, ddagrab is throttled to ~17fps on HAGS 144Hz displays. -->
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">PerMonitorV2</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </windowsSettings>
  </application>

  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>"#;

    let windows_attrs = tauri_build::WindowsAttributes::new()
        .app_manifest(manifest);

    let attrs = tauri_build::Attributes::new()
        .windows_attributes(windows_attrs);

    tauri_build::try_build(attrs).expect("failed to run tauri-build");
}
